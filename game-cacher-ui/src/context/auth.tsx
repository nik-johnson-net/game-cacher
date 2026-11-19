import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { authStatus, errorMessage } from "@/api/client";
import type { AuthStateResponse } from "@/api/types";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Loading } from "@/components/ui/loading";

type AuthApi = {
  state: AuthStateResponse;
  /** Re-reads GET /api/auth/status (after login/logout). */
  refresh: () => Promise<AuthStateResponse>;
};

const AuthContext = createContext<AuthApi | null>(null);

/** Checks GET /api/auth/status on boot; children render once it is known. */
export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthStateResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);

  const refresh = useCallback(async () => {
    const next = await authStatus();
    setState(next);
    return next;
  }, []);

  useEffect(() => {
    let cancelled = false;
    authStatus()
      .then((s) => {
        if (!cancelled) {
          setState(s);
          setError(null);
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [attempt]);

  const api = useMemo(() => (state ? { state, refresh } : null), [state, refresh]);

  if (!api) {
    return error ? (
      <div className="p-8 max-w-xl mx-auto">
        <AlertBanner variant="error" title="Cannot reach game-cacher" message={error} onRetry={() => setAttempt((n) => n + 1)} />
      </div>
    ) : (
      <Loading className="h-screen" />
    );
  }
  return <AuthContext.Provider value={api}>{children}</AuthContext.Provider>;
}

// eslint-disable-next-line react-refresh/only-export-components
export function useAuth(): AuthApi {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used inside <AuthProvider>");
  return ctx;
}
