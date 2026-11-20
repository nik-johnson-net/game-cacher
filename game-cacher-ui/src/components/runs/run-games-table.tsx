import type { GameRun } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { ProgressBar } from "@/components/ui/progress-bar";
import { StatusBadge } from "@/components/ui/status-badge";
import { Table, THead, Th, Tr, Td, TableMessageRow } from "@/components/ui/table";
import { formatBytes, formatDuration, formatInt, formatRate, percent } from "@/lib/format";
import { gameStatusSpec } from "@/lib/status";

/** Per-game rows of a run: status, version change, download progress, rate/ETA, totals. */
export function RunGamesTable({ games }: { games: GameRun[] }) {
  return (
    <Table>
      <THead>
        <tr>
          <Th>Game</Th>
          <Th>Status</Th>
          <Th>Version</Th>
          <Th>Progress</Th>
          <Th className="text-right">Rate</Th>
          <Th className="text-right">ETA</Th>
          <Th className="text-right">Downloaded</Th>
          <Th className="text-right">Already cached</Th>
          <Th>Error</Th>
        </tr>
      </THead>
      <tbody>
        {games.length === 0 && (
          <TableMessageRow colSpan={9}>
            <p className="p-4 text-sm text-text-secondary">No games were selected in this run.</p>
          </TableMessageRow>
        )}
        {games.map((g) => (
          <Tr key={g.game_id}>
            <Td>
              {g.name}
              <div className="text-xs text-text-muted break-all">{g.game_id}</div>
            </Td>
            <Td>
              <StatusBadge spec={gameStatusSpec(g.status, g.updated, g.error)} />
            </Td>
            <Td className="whitespace-nowrap">
              {g.current_version === null && g.previous_version === null ? (
                <span className="text-text-faint">—</span>
              ) : (
                <>
                  <span className="text-text-secondary">{g.previous_version ?? "—"}</span>
                  <span className="text-text-faint"> → </span>
                  <Badge variant={g.updated ? "default" : "secondary"}>{g.current_version ?? "—"}</Badge>
                </>
              )}
            </Td>
            <Td>
              <GameProgress game={g} />
            </Td>
            <Td className="text-right tabular-nums whitespace-nowrap">{g.status === "downloading" ? formatRate(g.rate_bps) : ""}</Td>
            <Td className="text-right tabular-nums whitespace-nowrap">{g.status === "downloading" ? formatDuration(g.eta_secs) : ""}</Td>
            <Td className="text-right tabular-nums">{formatBytes(g.bytes_downloaded)}</Td>
            <Td className="text-right tabular-nums">{formatBytes(g.bytes_already_cached)}</Td>
            <Td className="text-status-error max-w-xs break-words">{g.error ?? ""}</Td>
          </Tr>
        ))}
      </tbody>
    </Table>
  );
}

function GameProgress({ game: g }: { game: GameRun }) {
  const files = <div className="text-xs text-text-muted tabular-nums">{formatInt(g.objects_done)}/{formatInt(g.objects)} files</div>;
  switch (g.status) {
    case "pending":
    case "checking":
    case "cancelled":
      return <span className="text-text-faint">—</span>;
    case "queued":
      return (
        <span className="text-xs text-text-secondary tabular-nums whitespace-nowrap">
          {g.bytes_to_download ? `${formatBytes(g.bytes_to_download)} to download` : "nothing to download"}
        </span>
      );
    case "downloading":
      return (
        <div className="min-w-40">
          <ProgressBar value={percent(g.bytes_downloaded, g.bytes_to_download ?? 0)} trackClassName="w-28" label={`${g.name} progress`} />
          <div className="text-xs text-text-secondary tabular-nums whitespace-nowrap">
            {formatBytes(g.bytes_downloaded)} / {formatBytes(g.bytes_to_download)}
          </div>
          {files}
        </div>
      );
    case "done":
    case "failed":
      return g.objects > 0 ? files : <span className="text-text-faint">—</span>;
  }
}
