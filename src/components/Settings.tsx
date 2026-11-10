import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useState } from "react";
import { Check, KeyRound, LockKeyhole, Loader2, X } from "lucide-react";
import { api, useStore } from "../lib/store";
import { Button } from "./ui/button";
import type { Config } from "../../shared/types";
export default function Settings() {
  const open = useStore((s) => s.settings);
  const config = useStore((s) => s.config);
  const [key, setKey] = useState("");
  const [model, setModel] = useState("subq-preview");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  useEffect(() => {
    if (open) {
      setModel(config?.model || "subq-preview");
      setStatus("");
      setError("");
    } else setKey("");
  }, [open, config?.model]);
  async function save() {
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
        max_steps: config?.max_steps ?? 20,
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
          `${model} is not available to this key. SubQ Preview needs early-access permission.`,
        );
      setStatus(`Connected · ${model} is available`);
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
          setModel(config?.model || "subq-preview");
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
            <option value="subq-preview">SubQ Preview</option>
          </select>
          <p className="field-help">
            Preview supports generation and tools. Organization early access is
            required. Snap is a decision model.
          </p>
          <div className="permission-note">
            <strong>You're in control</strong>
            <p>
              Every edit and shell command asks for approval. Commands run on
              your Mac with your user permissions; this version does not provide
              an OS sandbox.
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
              onClick={() => {
                void api
                  .setApiKey("")
                  .then(async () => {
                    useStore.setState({
                      config: await api.rpc<Config>("config.get"),
                    });
                    setStatus("Saved key removed");
                  })
                  .catch((e) => setError(e.message));
              }}
            >
              Remove saved key
            </button>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
