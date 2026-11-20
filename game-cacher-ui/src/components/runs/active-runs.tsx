import { Link } from "react-router";
import type { PlatformSummary } from "@/api/types";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Loading } from "@/components/ui/loading";
import { useRuns } from "@/hooks/useRuns";
import { platformName } from "@/lib/runs";
import { CancelRunButton } from "./cancel-run-button";
import { RunSummary } from "./run-summary";

const IDLE_POLL_MS = 5000;

/** Dashboard card: progress of every running update run, with a link to the history. */
export function ActiveRuns({ platforms }: { platforms: PlatformSummary[] | undefined }) {
  const { data, error, loading, refresh, active } = useRuns(IDLE_POLL_MS);

  return (
    <Card className="mt-6">
      <CardHeader
        title="Active update runs"
        description={
          <Link to="/runs" className="text-accent hover:text-accent-hover">
            View run history
          </Link>
        }
      />
      <CardBody className="space-y-4">
        {error && <AlertBanner variant="error" message={error.message} onRetry={refresh} />}
        {loading && !data && <Loading />}
        {data && active.length === 0 && <p className="text-sm text-text-secondary">No update runs in progress.</p>}
        {active.map((run) => (
          <div key={run.id} className="flex items-start gap-3 border-b border-border-muted last:border-b-0 pb-4 last:pb-0">
            <div className="flex-1 min-w-0">
              <RunSummary
                run={run}
                title={
                  <Link to={`/platforms/${encodeURIComponent(run.platform)}`} className="text-accent hover:text-accent-hover font-semibold">
                    {platformName(platforms, run.platform)}
                  </Link>
                }
              />
            </div>
            <CancelRunButton platformId={run.platform} onChanged={refresh} />
          </div>
        ))}
      </CardBody>
    </Card>
  );
}
