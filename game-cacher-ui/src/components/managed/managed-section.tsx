import { useCallback, useEffect, useState } from "react";
import { getManaged } from "@/api/client";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Loading } from "@/components/ui/loading";
import { usePolling } from "@/hooks/usePolling";
import { AuthPanel } from "./auth-panel";
import { GamesTable } from "./games-table";
import { ManualList } from "./manual-list";
import { RunPanel } from "./run-panel";

const IDLE_POLL_MS = 15000;
const RUNNING_POLL_MS = 2000;

/** Managed updates: sign-in, game selection and update runs for one platform. */
export function ManagedSection({ platformId }: { platformId: string }) {
  const load = useCallback((signal: AbortSignal) => getManaged(platformId, signal), [platformId]);
  const [fast, setFast] = useState(false);
  const { data, error, loading, refresh, setData } = usePolling(load, fast ? RUNNING_POLL_MS : IDLE_POLL_MS);

  // Poll quickly while a run is in progress.
  const running = data?.running ?? false;
  useEffect(() => setFast(running), [running]);

  return (
    <section className="mt-8 space-y-4">
      <h2 className="text-lg font-semibold">Managed updates</h2>
      {error && <AlertBanner variant="error" message={error.message} onRetry={refresh} />}
      {loading && !data && <Loading />}
      {data && (
        <>
          {data.login_methods.includes("manual_list") ? (
            <ManualList key={data.selected.join("\n")} platformId={platformId} saved={data.selected} onSaved={setData} />
          ) : (
            <>
              <AuthPanel platformId={platformId} status={data} onChanged={refresh} />
              {(data.auth.logged_in || !data.auth.required) && (
                <GamesTable
                  platformId={platformId}
                  saved={data.selected}
                  accountKey={data.auth.account ?? ""}
                  onSaved={setData}
                />
              )}
            </>
          )}
          <RunPanel platformId={platformId} status={data} onChanged={refresh} />
        </>
      )}
    </section>
  );
}
