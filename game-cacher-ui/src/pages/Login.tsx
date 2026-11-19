import { useState, type FormEvent } from "react";
import { Navigate, useNavigate } from "react-router";
import { ApiError, errorMessage, login } from "@/api/client";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Spinner } from "@/components/ui/loading";
import { useAuth } from "@/context/auth";

export default function Login() {
  const { state, refresh } = useAuth();
  const navigate = useNavigate();
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!state.auth_enabled || state.authenticated) return <Navigate to="/" replace />;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await login(password);
      await refresh();
      navigate("/", { replace: true });
    } catch (err) {
      setError(err instanceof ApiError && err.status === 401 ? "Incorrect password." : errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="min-h-screen flex items-center justify-center p-6 bg-bg-base">
      <form onSubmit={submit} className="w-full max-w-sm bg-bg-surface border border-border p-6 space-y-4">
        <div>
          <div className="text-xl font-bold text-accent leading-none">GC</div>
          <div className="text-xs text-text-secondary uppercase mt-1 tracking-[3px]">game cacher</div>
        </div>
        <AlertBanner variant="error" message={error} />
        <Field label="Admin password" htmlFor="password">
          <Input
            id="password"
            type="password"
            autoFocus
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
        <Button type="submit" className="w-full" disabled={busy || !password}>
          {busy && <Spinner />}
          Sign in
        </Button>
      </form>
    </div>
  );
}
