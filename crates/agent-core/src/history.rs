use anyhow::Result;
use serde_json::Value;
use std::borrow::Cow;

const BUDGET: usize = 512 * 1024;
const EXCERPT_BYTES: usize = 12 * 1024;

pub(crate) struct ModelHistory<'a> {
    pub items: Cow<'a, [Value]>,
    pub stored_bytes: usize,
    pub sent_bytes: usize,
}

/// Keep the latest request and complete tool exchanges within the generation
/// budget. The original transcript remains available to storage and to the
/// file-version checks.
pub(crate) fn for_generation(input: &[Value]) -> Result<ModelHistory<'_>> {
    let stored_bytes = serde_json::to_vec(input)?.len();
    if stored_bytes <= BUDGET {
        return Ok(ModelHistory {
            items: Cow::Borrowed(input),
            stored_bytes,
            sent_bytes: stored_bytes,
        });
    }

    let latest_user = input.iter().rposition(|item| item["role"] == "user");
    let mut used = 2; // JSON array brackets.
    let mut groups = Vec::new();
    if let Some(index) = latest_user {
        let task = &input[index..index + 1];
        used += serde_json::to_vec(task)?.len() - 1;
        groups.push((index, Cow::Borrowed(task)));
    }

    // A model turn stores all calls before their results. Keep those together
    // so Chat Completions never receives a call without its output.
    let mut end = input.len();
    for start in (0..input.len()).rev() {
        let begins_exchange = start == 0
            || input[start]["role"] == "user"
            || input[start]["type"] == "message"
            || input[start - 1]["role"] == "user"
            || (input[start - 1]["type"] == "function_call_output"
                && input[start]["type"] != "function_call_output");
        if !begins_exchange {
            continue;
        }
        let group = &input[start..end];
        end = start;
        if latest_user == Some(start) {
            continue;
        }
        let bytes = serde_json::to_vec(group)?.len() - 1;
        if used + bytes <= BUDGET {
            groups.push((start, Cow::Borrowed(group)));
            used += bytes;
        } else if used < BUDGET {
            let shorter = excerpt_results(group);
            let bytes = serde_json::to_vec(&shorter)?.len() - 1;
            if used + bytes <= BUDGET {
                used += bytes;
                groups.push((start, Cow::Owned(shorter)));
            }
        }
    }
    groups.sort_unstable_by_key(|(start, _)| *start);
    let items: Vec<_> = groups
        .into_iter()
        .flat_map(|(_, group)| group.into_owned())
        .collect();
    Ok(ModelHistory {
        sent_bytes: serde_json::to_vec(&items)?.len(),
        items: Cow::Owned(items),
        stored_bytes,
    })
}

fn excerpt_results(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            if item["type"] == "function_call_output"
                && let Some(output) = item["output"].as_str().filter(|s| s.len() > EXCERPT_BYTES)
            {
                let prefix = agent_context::bounded(output, EXCERPT_BYTES / 2);
                let mut tail_start = output.len() - EXCERPT_BYTES / 2;
                while !output.is_char_boundary(tail_start) {
                    tail_start += 1;
                }
                item["output"] = Value::String(format!(
                    "{prefix}\n[Earlier tool output excerpt. {} bytes omitted.]\n{}",
                    tail_start - prefix.len(),
                    &output[tail_start..],
                ));
            }
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounds_large_history_without_losing_the_task_or_splitting_tool_exchanges() {
        let task = json!({"role":"user","content":"Fix the behavior in this checkout."});
        let mut input = vec![task.clone()];
        for index in 0..9 {
            input.push(json!({"type":"function_call","call_id":index.to_string(),
                "name":"read_file","arguments":"{\"path\":\"file.py\"}"}));
            input.push(
                json!({"type":"function_call_output","call_id":index.to_string(),
                "output":"世界".repeat(28_000)}),
            );
        }
        let history = for_generation(&input).unwrap();
        assert!(history.stored_bytes > BUDGET && history.sent_bytes <= BUDGET);
        assert_eq!(history.items[0], task);
        assert_eq!(history.items.last(), input.last());
        assert!(history.items.iter().any(|item| {
            item["output"]
                .as_str()
                .is_some_and(|s| s.contains("bytes omitted"))
        }));
        for pair in history.items[1..].chunks_exact(2) {
            assert_eq!(pair[0]["type"], "function_call");
            assert_eq!(pair[1]["type"], "function_call_output");
            assert_eq!(pair[0]["call_id"], pair[1]["call_id"]);
        }
        assert_eq!(input.len(), 19);
    }

    #[test]
    fn leaves_history_under_budget_unchanged() {
        let input = [json!({"role":"user","content":"Fix this task"})];
        let history = for_generation(&input).unwrap();
        assert!(matches!(history.items, Cow::Borrowed(_)));
        assert_eq!(history.sent_bytes, history.stored_bytes);
    }
}
