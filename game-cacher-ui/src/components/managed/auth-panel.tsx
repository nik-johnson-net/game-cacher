import { useEffect, useRef, useState, type FormEvent } from "react";
import { QRCodeSVG } from "qrcode.react";
import { KeyRound, LogOut, QrCode, Smartphone } from "lucide-react";
import { errorMessage, managedLogin, managedLogout } from "@/api/client";
import type { GuardType, LoginMethod, LoginResponse, ManagedStatus } from "@/api/types";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { ExternalLink } from "@/components/ui/external-link";
import { Field, Input } from "@/components/ui/form";
import { Spinner } from "@/components/ui/loading";
import { StatusBadge } from "@/components/ui/status-badge";
import { useToast } from "@/context/toast";
import { loginSpec } from "@/lib/status";
import { extractAuthCode } from "@/lib/utils";

type Stage =
  | { kind: "idle" }
  | { kind: "qr"; challengeId: string; url: string }
  | { kind: "guard"; challengeId: string; guardType: GuardType }
  | { kind: "device"; challengeId: string };

const POLL_MS = 2000;

interface AuthPanelProps {
  platformId: string;
  status: ManagedStatus;
  /** Re-fetch the managed status after a login state change. */
  onChanged: () => void;
}

export function AuthPanel({ platformId, status, onChanged }: AuthPanelProps) {
  const toast = useToast();
  const [stage, setStage] = useState<Stage>({ kind: "idle" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const methods = status.login_methods;
  const has = (m: LoginMethod) => methods.includes(m);

  /** Applies a login response to the local state machine. */
  const apply = (res: LoginResponse) => {
    switch (res.status) {
      case "logged_in":
        setStage({ kind: "idle" });
        setError(null);
        toast.push({ variant: "success", message: `Signed in as ${res.account}.` });
        onChanged();
        break;
      case "qr":
        setStage({ kind: "qr", challengeId: res.challenge_id, url: res.url });
        break;
      case "pending":
        break; // keep waiting; the poll loop schedules the next check
      case "guard_required":
        // "email" / "device" (authenticator code): show the code form.
        // "device_confirmation": approve on the phone and keep polling qr_poll.
        setStage(
          res.guard_type === "email" || res.guard_type === "device"
            ? { kind: "guard", challengeId: res.challenge_id, guardType: res.guard_type }
            : { kind: "device", challengeId: res.challenge_id },
        );
        break;
      case "failed":
        setStage({ kind: "idle" });
        setError(res.message);
        break;
    }
  };

  const call = async (req: Parameters<typeof managedLogin>[1]) => {
    setBusy(true);
    setError(null);
    try {
      apply(await managedLogin(platformId, req));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const signOut = async () => {
    setBusy(true);
    try {
      await managedLogout(platformId);
      setStage({ kind: "idle" });
      onChanged();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  // Poll qr_poll every 2s while waiting for a QR scan or a device approval.
  const challengeId = stage.kind === "qr" || stage.kind === "device" ? stage.challengeId : null;
  const applyRef = useRef(apply);
  useEffect(() => {
    applyRef.current = apply;
  });
  useEffect(() => {
    if (!challengeId) return;
    let cancelled = false;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const res = await managedLogin(platformId, { method: "qr_poll", challenge_id: challengeId });
        if (cancelled) return;
        applyRef.current(res);
        if (res.status === "pending" || res.status === "qr") timer = window.setTimeout(tick, POLL_MS);
      } catch (e) {
        if (cancelled) return;
        setError(errorMessage(e));
        setStage({ kind: "idle" });
      }
    };
    timer = window.setTimeout(tick, POLL_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [platformId, challengeId]);

  const { auth } = status;
  const noAuth = has("none") || (!auth.required && methods.every((m) => m === "none" || m === "manual_list"));

  return (
    <Card>
      <CardHeader
        title="Sign-in"
        actions={!noAuth ? <StatusBadge spec={loginSpec(auth.logged_in)} /> : undefined}
      />
      <CardBody className="space-y-4">
        <AlertBanner variant="error" message={error} />
        {auth.logged_in ? (
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="text-sm">
              Signed in as <strong className="font-semibold">{auth.account ?? "unknown account"}</strong>
            </div>
            <Button variant="secondary" size="sm" onClick={() => void signOut()} disabled={busy}>
              {busy ? <Spinner /> : <LogOut />}
              Sign out
            </Button>
          </div>
        ) : noAuth ? (
          <p className="text-sm text-text-secondary">No sign-in required.</p>
        ) : (
          <>
            {has("qr") && (
              <QrSection
                stage={stage}
                busy={busy}
                onStart={() => void call({ method: "qr" })}
                onCancel={() => setStage({ kind: "idle" })}
              />
            )}
            {has("password") && (
              <PasswordSection
                stage={stage}
                busy={busy}
                onPassword={(username, password) => void call({ method: "password", username, password })}
                onGuard={(code) => stage.kind === "guard" && void call({ method: "guard", challenge_id: stage.challengeId, code })}
                onCancel={() => setStage({ kind: "idle" })}
              />
            )}
            {has("auth_code") && (
              <AuthCodeSection
                authUrl={auth.auth_url ?? null}
                busy={busy}
                onSubmit={(code) => void call({ method: "auth_code", code })}
              />
            )}
          </>
        )}
      </CardBody>
    </Card>
  );
}

function QrSection({ stage, busy, onStart, onCancel }: { stage: Stage; busy: boolean; onStart: () => void; onCancel: () => void }) {
  if (stage.kind === "qr") {
    return (
      <div className="flex flex-wrap items-start gap-4">
        <div className="bg-text-primary p-3 w-fit">
          <QRCodeSVG value={stage.url} size={176} fgColor="var(--color-bg-base)" bgColor="var(--color-text-primary)" />
        </div>
        <div className="text-sm text-text-secondary space-y-2 max-w-sm">
          <div className="flex items-center gap-2 text-text-primary">
            <Spinner /> Waiting for approval...
          </div>
          <p>Scan this code with the platform's mobile app and approve the sign-in.</p>
          <Button variant="ghost" size="sm" onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </div>
    );
  }
  return (
    <div>
      <Button size="sm" onClick={onStart} disabled={busy || stage.kind !== "idle"}>
        {busy ? <Spinner /> : <QrCode />}
        Sign in with QR
      </Button>
    </div>
  );
}

function PasswordSection({
  stage,
  busy,
  onPassword,
  onGuard,
  onCancel,
}: {
  stage: Stage;
  busy: boolean;
  onPassword: (username: string, password: string) => void;
  onGuard: (code: string) => void;
  onCancel: () => void;
}) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");

  if (stage.kind === "device") {
    return (
      <div className="flex items-start gap-3 text-sm">
        <Smartphone className="size-4 mt-0.5 text-status-active shrink-0" />
        <div className="space-y-2">
          <div className="flex items-center gap-2 text-text-primary">
            <Spinner /> Approve the sign-in on your phone.
          </div>
          <p className="text-text-secondary">This page continues automatically once you confirm.</p>
          <Button variant="ghost" size="sm" onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </div>
    );
  }

  if (stage.kind === "guard") {
    const submit = (e: FormEvent) => {
      e.preventDefault();
      onGuard(code.trim());
    };
    return (
      <form onSubmit={submit} className="space-y-3 max-w-xs">
        <Field
          label="Guard code"
          htmlFor="guard-code"
          hint={stage.guardType === "email" ? "Enter the code emailed to you." : "Enter the code from your authenticator."}
        >
          <Input id="guard-code" autoFocus autoComplete="one-time-code" value={code} onChange={(e) => setCode(e.target.value)} />
        </Field>
        <div className="flex gap-2">
          <Button type="submit" size="sm" disabled={busy || !code.trim()}>
            {busy && <Spinner />}
            Submit code
          </Button>
          <Button variant="ghost" size="sm" onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </form>
    );
  }

  const submit = (e: FormEvent) => {
    e.preventDefault();
    onPassword(username.trim(), password);
  };
  return (
    <form onSubmit={submit} className="space-y-3 max-w-xs">
      <Field label="Username" htmlFor="gc-username">
        <Input id="gc-username" autoComplete="username" value={username} onChange={(e) => setUsername(e.target.value)} />
      </Field>
      <Field label="Password" htmlFor="gc-password">
        <Input id="gc-password" type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
      </Field>
      <Button type="submit" size="sm" disabled={busy || stage.kind !== "idle" || !username.trim() || !password}>
        {busy && <Spinner />}
        Sign in
      </Button>
    </form>
  );
}

function AuthCodeSection({ authUrl, busy, onSubmit }: { authUrl: string | null; busy: boolean; onSubmit: (code: string) => void }) {
  const [raw, setRaw] = useState("");
  const code = extractAuthCode(raw);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (code) onSubmit(code);
  };
  return (
    <form onSubmit={submit} className="space-y-3 max-w-lg">
      <p className="text-sm text-text-secondary">
        {authUrl ? (
          <>
            <ExternalLink href={authUrl}>Open the sign-in page</ExternalLink>, log in, then paste the authorization code
            (or the whole JSON response) below.
          </>
        ) : (
          "Obtain an authorization code from the platform, then paste it below."
        )}
      </p>
      <Field label="Authorization code" htmlFor="auth-code" hint='Accepts the raw code or {"authorizationCode": "..."}.'>
        <Input id="auth-code" autoComplete="off" value={raw} onChange={(e) => setRaw(e.target.value)} />
      </Field>
      <Button type="submit" size="sm" disabled={busy || !code}>
        {busy ? <Spinner /> : <KeyRound />}
        Sign in
      </Button>
    </form>
  );
}
