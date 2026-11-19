import { useState } from "react";
import { Play } from "lucide-react";
import { ApiError, errorMessage, runManaged } from "@/api/client";
import type { ManagedStatus } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Spinner } from "@/components/ui/loading";
import { StatusBadge } from "@/components/ui/status-badge";
import { Table, THead, Th, Tr, Td } from "@/components/ui/table";
import { useToast } from "@/context/toast";
import { formatBytes, formatRelativeTime, formatTimestamp } from "@/lib/format";
import { runningSpec, updatedSpec } from "@/lib/status";

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
        toast.push({ variant: "info", message: "An update run is already running." });
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
          </span>
        }
        actions={
          <>
            <StatusBadge spec={runningSpec(running)} />
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
            <div className="text-xs text-text-secondary" title={formatTimestamp(run.started)}>
              Last run started {formatRelativeTime(run.started)}
              {run.finished ? `, finished ${formatRelativeTime(run.finished)}` : ", still in progress"}
            </div>
            <Table>
              <THead>
                <tr>
                  <Th>Game</Th>
                  <Th>Version</Th>
                  <Th>Result</Th>
                  <Th className="text-right">Downloaded</Th>
                  <Th className="text-right">Already cached</Th>
                  <Th className="text-right">Files</Th>
                  <Th>Error</Th>
                </tr>
              </THead>
              <tbody>
                {run.games.length === 0 && (
                  <tr>
                    <td colSpan={7} className="p-4 text-sm text-text-secondary">
                      Nothing was selected in this run.
                    </td>
                  </tr>
                )}
                {run.games.map((g) => (
                  <Tr key={g.game_id}>
                    <Td>
                      {g.name}
                      <div className="text-xs text-text-muted">{g.game_id}</div>
                    </Td>
                    <Td className="whitespace-nowrap">
                      <span className="text-text-secondary">{g.previous_version ?? "—"}</span>
                      <span className="text-text-faint"> → </span>
                      <Badge variant={g.updated ? "default" : "secondary"}>{g.current_version ?? "—"}</Badge>
                    </Td>
                    <Td>
                      <StatusBadge spec={updatedSpec(g.updated, g.error)} />
                    </Td>
                    <Td className="text-right tabular-nums">{formatBytes(g.bytes_downloaded)}</Td>
                    <Td className="text-right tabular-nums">{formatBytes(g.bytes_already_cached)}</Td>
                    <Td className="text-right tabular-nums">{g.files}</Td>
                    <Td className="text-status-error max-w-xs break-words">{g.error ?? ""}</Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          </>
        ) : (
          <p className="text-sm text-text-secondary">No runs yet.</p>
        )}
      </CardBody>
    </Card>
  );
}
