import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useState } from "react";
import { Check, KeyRound, LockKeyhole, Loader2, X } from "lucide-react";
import { api, useStore } from "../lib/store";
import { Button } from "./ui/button";
import type { Config } from "../../shared/types";

const DEFAULT_MODEL_TURNS = 64;
const MAX_MODEL_TURNS = 128;

export default function Settings() {
  const open = useStore((s) => s.settings);
  const config = useStore((s) => s.config);
  const [key, setKey] = useState("");
  const [model, setModel] = useState("subq-savant-1.4");
  const [budget, setBudget] = useState(512 * 1024);
  const [turnLimit, setTurnLimit] = useState(String(DEFAULT_MODEL_TURNS));
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    if (open) {
      setModel(config?.model || "subq-savant-1.4");
      setBudget(config?.context_budget_bytes || 512 * 1024);
      setTurnLimit(String(config?.max_steps ?? DEFAULT_MODEL_TURNS));
      setStatus("");
      setError("");
    } else {
      setKey("");
    }
  }, [open, config?.model, config?.context_budget_bytes, config?.max_steps]);
  async function save() {
    const turns = Number(turnLimit);
    if (!Number.isInteger(turns) || turns < 1 || turns > MAX_MODEL_TURNS) {
      setStatus("");
      setError("Enter a model turn limit from 1 to 128.");
      return;
    }
    setBusy(true);
    setError("");
    setStatus("");
    try {
      if (key.trim()) {
        await api.setApiKey(key.trim());
        setKey("");
      }
      await api.rpc("config.update", {
        model,
        max_steps: turns,
        context_budget_bytes: budget,
      });
      const next = await api.rpc<Config>("config.get");
      useStore.setState({ config: next });
      setStatus("Settings saved");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function test() {
    setBusy(true);
    setError("");
    setStatus("");
    try {
      if (key.trim()) {
        await api.setApiKey(key.trim());
        setKey("");
        useStore.setState({ config: await api.rpc<Config>("config.get") });
      }
      const models = await api.rpc<{ id: string }[]>("models.list");
      if (!models.some((m) => m.id === model))
        throw new Error(
          `${model} is not available to this key. SubQ Savant needs early-access permission.`,
        );
      setStatus(`Connected · ${model} is available`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function removeKey() {
    setBusy(true);
    setError("");
    setStatus("");
    try {
      await api.setApiKey("");
      setKey("");
      useStore.setState({ config: await api.rpc<Config>("config.get") });
      setStatus("Saved key removed");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog.Root
      open={open}
      onOpenChange={(settings) => {
        useStore.setState({ settings });
        if (settings) {
          setModel(config?.model || "subq-savant-1.4");
          setStatus("");
          setError("");
        }
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="modal-overlay" />
        <Dialog.Content className="settings-modal">
          <div className="modal-top">
            <span className="eyebrow">YOUR WORKSPACE, YOUR WAY</span>
            <Dialog.Close className="icon-button" aria-label="Close settings">
              <X size={18} />
            </Dialog.Close>
          </div>
          <Dialog.Title>Make the connection.</Dialog.Title>
          <Dialog.Description>
            Connect Substrate to your Subquadratic account.
          </Dialog.Description>
          <div className="provider-card">
            <div className="provider-symbol">
              S<span>Q</span>
            </div>
            <div>
              <strong>Subquadratic</strong>
              <small>Long context. More room to think.</small>
            </div>
            <span className="provider-tag">PROVIDER</span>
          </div>
          <label htmlFor="api-key">
            API key{" "}
            <span>
              {config?.has_key ? "A key is securely stored" : "Required"}
            </span>
          </label>
          <div className="key-field">
            <KeyRound size={16} />
            <input
              id="api-key"
              type="password"
              autoComplete="off"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder={
                config?.has_key
                  ? "Enter a new key to replace the saved key"
                  : "Enter your Subquadratic API key"
              }
            />
          </div>
          <p className="field-help">
            <LockKeyhole size={12} /> Encrypted with your operating system.
            Never returned to the interface.
          </p>
          <label htmlFor="model">Agent model</label>
          <select
            id="model"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          >
            <option value="subq-savant-1.4">SubQ Savant 1.4</option>
          </select>
          <p className="field-help">
            Savant supports generation and tools. Organization early access is
            required. Snap ranks repository files for each task.
          </p>
          <label htmlFor="context-budget">Repository context budget</label>
          <select
            id="context-budget"
            value={budget}
            onChange={(event) => setBudget(Number(event.target.value))}
          >
            {[32, 128, 256, 512, 1024, 2048, 4096].map((kib) => (
              <option key={kib} value={kib * 1024}>
                {kib >= 1024 ? `${kib / 1024} MiB` : `${kib} KiB`}
              </option>
            ))}
          </select>
          <p className="field-help">
            Tasks and eligible file excerpts go to Snap for selection; selected
            source goes to Savant. Sensitive and ignored paths are excluded.
            Conversation history has its own budget.
          </p>
          <label htmlFor="model-turn-limit">Model turns per task</label>
          <input
            id="model-turn-limit"
            type="number"
            min={1}
            max={MAX_MODEL_TURNS}
            step={1}
            value={turnLimit}
            onChange={(event) => setTurnLimit(event.target.value)}
            aria-describedby="model-turn-limit-help"
          />
          <p className="field-help" id="model-turn-limit-help">
            Applies to new tasks. The default is {DEFAULT_MODEL_TURNS} turns.
          </p>
          <div className="permission-note">
            <strong>You're in control</strong>
            <p>
              Use the composer to choose whether edits ask for approval. Shell
              commands always ask for approval and run on your Mac with your
              user permissions.
            </p>
          </div>
          {error && (
            <p className="form-error" role="alert">
              {error}
            </p>
          )}
          {status && (
            <p className="form-success">
              <Check size={14} />
              {status}
            </p>
          )}
          <div className="modal-actions">
            <Button
              variant="outline"
              onClick={() => void test()}
              disabled={busy}
            >
              {busy && <Loader2 className="spin" size={14} />}Test connection
            </Button>
            <Button onClick={() => void save()} disabled={busy}>
              Save settings
            </Button>
          </div>
          {config?.has_key && (
            <button
              className="text-button remove-key"
              onClick={() => void removeKey()}
              disabled={busy}
            >
              Remove saved key
            </button>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
