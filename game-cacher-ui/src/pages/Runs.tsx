import { Fragment, useCallback, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { ChevronDown, ChevronRight, History } from "lucide-react";
import { listPlatforms } from "@/api/client";
import type { PlatformSummary, UpdateRun } from "@/api/types";
import { RunGamesTable } from "@/components/runs/run-games-table";
import { RunSummary } from "@/components/runs/run-summary";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { SkeletonRows } from "@/components/ui/loading";
import { PageHeader } from "@/components/ui/page-header";
import { StatusBadge } from "@/components/ui/status-badge";
import { Table, THead, Th, Tr, Td, TableMessageRow } from "@/components/ui/table";
import { usePolling } from "@/hooks/usePolling";
import { useRuns } from "@/hooks/useRuns";
import { formatBytes, formatDuration, formatRelativeTime, formatTimestamp } from "@/lib/format";
import { platformName, runDurationSecs } from "@/lib/runs";
import { runStatusSpec } from "@/lib/status";

const IDLE_POLL_MS = 15000;
const COLS = 7;

/** History of the last managed-update runs of every platform. */
export default function Runs() {
  const [params, setParams] = useSearchParams();
  const filter = params.get("platform");
  const loadPlatforms = useCallback((signal: AbortSignal) => listPlatforms(signal), []);
  const runs = useRuns(IDLE_POLL_MS);
  const platforms = usePolling(loadPlatforms, 0);
  const [open, setOpen] = useState<Set<string>>(new Set());

  const managed = (platforms.data ?? []).filter((p) => p.managed);
  const shown = (runs.data ?? []).filter((r) => !filter || r.platform === filter);
  const toggle = (id: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  return (
    <div>
      <PageHeader
        breadcrumbs={[{ label: "Dashboard", href: "/" }, { label: "Update runs" }]}
        title="Update runs"
        description="The last 10 managed-update runs of each platform. Select a run to see its games."
      />
      {runs.error && <AlertBanner variant="error" message={runs.error.message} onRetry={runs.refresh} className="mb-4" />}
      <PlatformFilter platforms={managed} value={filter} onChange={(id) => setParams(id ? { platform: id } : {})} />
      <Table>
        <THead>
          <tr>
            <Th className="w-8">
              <span className="sr-only">Expand</span>
            </Th>
            <Th>Platform</Th>
            <Th>Started</Th>
            <Th>Duration</Th>
            <Th>Status</Th>
            <Th className="text-right">Games</Th>
            <Th className="text-right">Downloaded</Th>
          </tr>
        </THead>
        <tbody>
          {runs.loading && !runs.data && <SkeletonRows cols={COLS} />}
          {runs.data && shown.length === 0 && (
            <TableMessageRow colSpan={COLS}>
              <EmptyState icon={History} title="No runs yet" description="Runs appear here once a managed platform has been updated." />
            </TableMessageRow>
          )}
          {shown.map((run) => (
            <RunRow key={run.id} run={run} name={platformName(managed, run.platform)} open={open.has(run.id)} onToggle={() => toggle(run.id)} />
          ))}
        </tbody>
      </Table>
    </div>
  );
}

function PlatformFilter({
  platforms,
  value,
  onChange,
}: {
  platforms: PlatformSummary[];
  value: string | null;
  onChange: (id: string | null) => void;
}) {
  const options: [string | null, string][] = [[null, "All"], ...platforms.map((p): [string, string] => [p.id, p.name])];
  return (
    <div className="flex flex-wrap gap-1 mb-3" role="group" aria-label="Filter by platform">
      {options.map(([id, label]) => (
        <Button key={id ?? "all"} size="xs" variant={value === id ? "default" : "secondary"} aria-pressed={value === id} onClick={() => onChange(id)}>
          {label}
        </Button>
      ))}
    </div>
  );
}

function RunRow({ run, name, open, onToggle }: { run: UpdateRun; name: string; open: boolean; onToggle: () => void }) {
  const duration = runDurationSecs(run);
  const failed = run.games.filter((g) => g.status === "failed").length;
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <Fragment>
      <Tr className="cursor-pointer" onClick={onToggle}>
        <Td>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-expanded={open}
            aria-label={open ? "Hide games" : "Show games"}
            onClick={(e) => {
              e.stopPropagation();
              onToggle();
            }}
          >
            <Chevron />
          </Button>
        </Td>
        <Td>
          <Link
            to={`/platforms/${encodeURIComponent(run.platform)}`}
            className="text-accent hover:text-accent-hover font-medium"
            onClick={(e) => e.stopPropagation()}
          >
            {name}
          </Link>
        </Td>
        <Td title={formatTimestamp(run.started)}>{formatRelativeTime(run.started)}</Td>
        <Td className="tabular-nums">{formatDuration(duration)}</Td>
        <Td>
          <StatusBadge spec={runStatusSpec(run.status, run.phase)} />
        </Td>
        <Td className="text-right tabular-nums">
          {run.games.length}
          {failed > 0 && <span className="text-status-error"> ({failed} failed)</span>}
        </Td>
        <Td className="text-right tabular-nums">{formatBytes(run.bytes_downloaded)}</Td>
      </Tr>
      {open && (
        <tr className="bg-bg-base border-b border-border-muted">
          <td colSpan={COLS} className="p-4 space-y-3">
            <RunSummary run={run} />
            <RunGamesTable games={run.games} />
          </td>
        </tr>
      )}
    </Fragment>
  );
}
