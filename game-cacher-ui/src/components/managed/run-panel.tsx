import { useState } from "react";
import { Link } from "react-router";
import { Play } from "lucide-react";
import { ApiError, errorMessage, runManaged } from "@/api/client";
import type { ManagedStatus } from "@/api/types";
import { CancelRunButton } from "@/components/runs/cancel-run-button";
import { RunGamesTable } from "@/components/runs/run-games-table";
import { RunSummary } from "@/components/runs/run-summary";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Spinner } from "@/components/ui/loading";
import { useToast } from "@/context/toast";
import { formatRelativeTime, formatTimestamp } from "@/lib/format";

export function RunPanel({ platformId, status, onChanged }: { platformId: string; status: ManagedStatus; onChanged: () => void }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const { last_run: run, next_run: next, running } = status;

  const start = async () => {
    setBusy(true);
    try {
      await runManaged(platformId);
      toast.push({ variant: "success", message: "Update run started." });
      onChanged();
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        // 409: already running, or the platform is disabled — the server says which.
        toast.push({ variant: "info", message: errorMessage(e) });
        onChanged();
      } else {
        toast.push({ variant: "error", title: "Could not start run", message: errorMessage(e) });
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHeader
        title="Update runs"
        description={
          <span title={formatTimestamp(next)}>
            {running ? "Running now" : next ? `Next run ${formatRelativeTime(next)}` : "No run scheduled"}
            {" · "}
            <Link to={`/runs?platform=${encodeURIComponent(platformId)}`} className="text-accent hover:text-accent-hover">
              History
            </Link>
          </span>
        }
        actions={
          <>
            {running && <CancelRunButton platformId={platformId} onChanged={onChanged} />}
            <Button size="sm" onClick={() => void start()} disabled={busy || running}>
              {busy || running ? <Spinner /> : <Play />}
              Run now
            </Button>
          </>
        }
      />
      <CardBody className="space-y-3">
        {run ? (
          <>
            <RunSummary run={run} title={<span className="text-text-primary font-semibold">{running ? "Current run" : "Last run"}</span>} />
            <RunGamesTable games={run.games} />
          </>
        ) : (
          <p className="text-sm text-text-secondary">No runs yet.</p>
        )}
      </CardBody>
    </Card>
  );
}
