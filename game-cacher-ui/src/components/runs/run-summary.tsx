import type { UpdateRun } from "@/api/types";
import { ProgressBar } from "@/components/ui/progress-bar";
import { StatusBadge } from "@/components/ui/status-badge";
import { formatBytes, formatDuration, formatRate, formatRelativeTime, formatTimestamp, percent } from "@/lib/format";
import { runDurationSecs } from "@/lib/runs";
import { runStatusSpec } from "@/lib/status";

/** Run-level progress: status/phase, overall bytes, rate, ETA and the game in progress. */
export function RunSummary({ run, title }: { run: UpdateRun; title?: React.ReactNode }) {
  const active = run.status === "running";
  const finished = run.games.filter((g) => g.status === "done" || g.status === "failed").length;
  const current = run.games.find((g) => g.status === "downloading" || g.status === "checking");
  const pending = run.games.filter((g) => g.status === "pending" || g.status === "queued").length;
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-text-secondary">
        {title}
        <StatusBadge spec={runStatusSpec(run.status, run.phase)} />
        <span title={formatTimestamp(run.started)}>started {formatRelativeTime(run.started)}</span>
        {run.finished && <span title={formatTimestamp(run.finished)}>took {formatDuration(runDurationSecs(run))}</span>}
        <span>
          {finished}/{run.games.length} games
        </span>
      </div>
      {active && (
        <>
          <ProgressBar
            value={percent(run.bytes_downloaded, run.bytes_to_download)}
            trackClassName="w-full h-2"
            className="w-full"
            label="Run progress"
          />
          <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-text-secondary tabular-nums">
            <span>
              {formatBytes(run.bytes_downloaded)} <span className="text-text-muted">of</span> {formatBytes(run.bytes_to_download)}
              {run.phase === "checking" && <span className="text-text-muted"> (still checking)</span>}
            </span>
            <span>{formatRate(run.rate_bps)}</span>
            <span>ETA {formatDuration(run.eta_secs)}</span>
            {current && (
              <span className="text-text-primary">
                {current.status === "checking" ? "checking" : "downloading"} {current.name}
              </span>
            )}
            {pending > 0 && <span>{pending} waiting</span>}
          </div>
        </>
      )}
    </div>
  );
}
