import { useCallback, useState } from "react";
import { Link } from "react-router";
import { errorMessage, listPlatforms, setPlatformEnabled } from "@/api/client";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/form";
import { PageHeader } from "@/components/ui/page-header";
import { SkeletonRows } from "@/components/ui/loading";
import { StatusBadge } from "@/components/ui/status-badge";
import { Table, THead, Th, Tr, Td, TableMessageRow } from "@/components/ui/table";
import { useToast } from "@/context/toast";
import { usePolling } from "@/hooks/usePolling";
import { formatBytes } from "@/lib/format";
import { enabledSpec } from "@/lib/status";
import type { PlatformSummary } from "@/api/types";

export default function Platforms() {
  const toast = useToast();
  const load = useCallback((signal: AbortSignal) => listPlatforms(signal), []);
  const { data, error, loading, refresh, setData } = usePolling(load, 5000);
  const [pending, setPending] = useState<string | null>(null);

  const toggle = async (p: PlatformSummary, enabled: boolean) => {
    setPending(p.id);
    try {
      const updated = await setPlatformEnabled(p.id, enabled);
      setData((data ?? []).map((x) => (x.id === updated.id ? updated : x)));
    } catch (e) {
      toast.push({ variant: "error", title: `Could not update ${p.name}`, message: errorMessage(e) });
    } finally {
      setPending(null);
    }
  };

  return (
    <div>
      <PageHeader title="Platforms" description="Enable the platforms whose downloads should be cached." />
      {error && <AlertBanner variant="error" message={error.message} onRetry={refresh} className="mb-4" />}
      <Table>
        <THead>
          <tr>
            <Th>Platform</Th>
            <Th>State</Th>
            <Th>Enabled</Th>
            <Th>Managed updates</Th>
            <Th className="text-right">Storage</Th>
          </tr>
        </THead>
        <tbody>
          {loading && !data && <SkeletonRows cols={5} />}
          {data?.length === 0 && (
            <TableMessageRow colSpan={5}>
              <p className="p-4 text-sm text-text-secondary">No platforms.</p>
            </TableMessageRow>
          )}
          {data?.map((p) => (
            <Tr key={p.id}>
              <Td>
                <Link to={`/platforms/${encodeURIComponent(p.id)}`} className="text-accent hover:text-accent-hover font-medium">
                  {p.name}
                </Link>
                <div className="text-xs text-text-muted">{p.description}</div>
              </Td>
              <Td>
                <StatusBadge spec={enabledSpec(p.enabled)} />
              </Td>
              <Td>
                <Switch
                  checked={p.enabled}
                  disabled={pending === p.id}
                  aria-label={`${p.enabled ? "Disable" : "Enable"} ${p.name}`}
                  onCheckedChange={(v) => void toggle(p, v)}
                />
              </Td>
              <Td>{p.managed ? <Badge>managed updates</Badge> : <span className="text-text-muted">—</span>}</Td>
              <Td className="text-right tabular-nums">{formatBytes(p.stats.storage_bytes)}</Td>
            </Tr>
          ))}
        </tbody>
      </Table>
    </div>
  );
}
