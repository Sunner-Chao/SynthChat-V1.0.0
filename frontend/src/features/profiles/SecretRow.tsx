import { KeyRound, Save, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { ProfileApiError, type SecretStatus } from "../../api/profiles";

function secretErrorMessage(error: unknown): string {
  if (error instanceof ProfileApiError) {
    if (error.status === 503) return "系统密钥链当前不可用。";
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  return "密钥操作失败，请重试。";
}

export function SecretRow({
  status,
  onPut,
  onDelete,
}: {
  status: SecretStatus;
  onPut: (value: string) => Promise<void>;
  onDelete: () => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!value || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onPut(value);
      setValue("");
    } catch (cause) {
      setError(secretErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!status.configured || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onDelete();
      setValue("");
    } catch (cause) {
      setError(secretErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="secret-row" data-secret-name={status.name}>
      <div className="secret-row-heading">
        <KeyRound aria-hidden="true" size={16} />
        <div>
          <strong>{status.name}</strong>
          <span>{status.configured ? "已安全存储" : "未配置"}</span>
        </div>
      </div>

      <form className="secret-row-form" onSubmit={(event) => void submit(event)}>
        <label className="sr-only" htmlFor={`secret-${status.name}`}>
          {status.name} 密钥值
        </label>
        <input
          autoComplete="new-password"
          disabled={busy}
          id={`secret-${status.name}`}
          maxLength={2_560}
          onChange={(event) => setValue(event.target.value)}
          placeholder={status.configured ? "输入新值以替换" : "输入密钥"}
          spellCheck={false}
          type="password"
          value={value}
        />
        <button
          aria-label={`保存 ${status.name}`}
          className="icon-button"
          disabled={busy || value.length === 0}
          title={`保存 ${status.name}`}
          type="submit"
        >
          <Save aria-hidden="true" size={16} />
        </button>
        <button
          aria-label={`删除 ${status.name}`}
          className="icon-button danger"
          disabled={busy || !status.configured}
          onClick={() => void remove()}
          title={`删除 ${status.name}`}
          type="button"
        >
          <Trash2 aria-hidden="true" size={16} />
        </button>
      </form>
      {error ? <p className="form-error" role="alert">{error}</p> : null}
    </div>
  );
}
