import { useCallback } from "react";
import { Link } from "react-router";
import { getStatus, listPlatforms } from "@/api/client";
import type { AppStatus, PlatformSummary } from "@/api/types";
import { KeyValue, Stat } from "@/components/stat";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Badge } from "@/components/ui/badge";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { SkeletonRows } from "@/components/ui/loading";
import { PageHeader } from "@/components/ui/page-header";
import { ProgressBar } from "@/components/ui/progress-bar";
import { StatusBadge, toneText } from "@/components/ui/status-badge";
import { Table, THead, Th, Tr, Td } from "@/components/ui/table";
import { usePolling } from "@/hooks/usePolling";
import { formatBytes, formatDuration, formatInt, formatRatio, hitRatio, percent } from "@/lib/format";
import { enabledSpec, hitRatioTone, onOffSpec } from "@/lib/status";
import { cn } from "@/lib/utils";

export default function Dashboard() {
  const loadStatus = useCallback((signal: AbortSignal) => getStatus(signal), []);
  const loadPlatforms = useCallback((signal: AbortSignal) => listPlatforms(signal), []);
  const status = usePolling(loadStatus, 5000);
  const platforms = usePolling(loadPlatforms, 5000);
  const error = status.error ?? platforms.error;

  const refresh = () => {
    status.refresh();
    platforms.refresh();
  };

  return (
    <div>
      <PageHeader
        title="Dashboard"
        description={status.data ? `game-cacher ${status.data.version}` : "Cache status and bandwidth savings."}
      />
      {error && <AlertBanner variant="error" message={error.message} onRetry={refresh} className="mb-4" />}

      <Totals platforms={platforms.data} />
      <div className="grid gap-6 lg:grid-cols-2 mt-6">
        <StorageCard status={status.data} />
        <ServicesCard status={status.data} />
      </div>
      <PlatformTable platforms={platforms.data} loading={platforms.loading} />
    </div>
  );
}

function Totals({ platforms }: { platforms: PlatformSummary[] | undefined }) {
  const sum = (f: (p: PlatformSummary) => number) => (platforms ?? []).reduce((a, p) => a + f(p), 0);
  const requests = sum((p) => p.stats.requests);
  const ratio = hitRatio(sum((p) => p.stats.hits), sum((p) => p.stats.misses));
  const fromCache = sum((p) => p.stats.bytes_from_cache);
  const fromUpstream = sum((p) => p.stats.bytes_from_upstream);
  const ready = platforms !== undefined;
  return (
    <div className="grid gap-4 grid-cols-2 lg:grid-cols-4">
      <Stat label="Requests" value={ready ? formatInt(requests) : "—"} />
      <Stat
        label="Hit ratio"
        value={<span className={toneText(hitRatioTone(ratio))}>{ready ? formatRatio(ratio) : "—"}</span>}
        hint="hits / (hits + misses)"
      />
      <Stat label="Bandwidth saved" value={ready ? formatBytes(fromCache) : "—"} hint="served from cache" />
      <Stat label="Fetched upstream" value={ready ? formatBytes(fromUpstream) : "—"} hint="served from upstream" />
    </div>
  );
}

function StorageCard({ status }: { status: AppStatus | undefined }) {
  const s = status?.storage;
  const bounded = s?.max_bytes != null;
  const pct = s && s.max_bytes ? percent(s.used_bytes, s.max_bytes) : 0;
  return (
    <Card>
      <CardHeader title="Storage" description={s ? `${s.backend} backend` : undefined} />
      <CardBody>
        {s ? (
          <>
            <div className="flex items-baseline justify-between gap-3 text-sm mb-2">
              <span className="tabular-nums">
                {formatBytes(s.used_bytes)} <span className="text-text-muted">of</span>{" "}
                {bounded ? formatBytes(s.max_bytes) : "unbounded"}
              </span>
              {bounded && <span className="text-text-secondary tabular-nums">{Math.round(pct)}%</span>}
            </div>
            <ProgressBar
              value={bounded ? pct : 0}
              tone={pct > 95 ? "error" : pct > 85 ? "warn" : "active"}
              showLabel={false}
              trackClassName="w-full h-2"
              className="w-full"
              label="Storage used"
            />
            <dl className="mt-4">
              <KeyValue label="Objects">{formatInt(s.objects)}</KeyValue>
              <KeyValue label="Backend">{s.backend}</KeyValue>
              <KeyValue label="Secrets">{status?.secrets_backend}</KeyValue>
            </dl>
          </>
        ) : (
          <p className="text-sm text-text-muted">Loading...</p>
        )}
      </CardBody>
    </Card>
  );
}

function ServicesCard({ status }: { status: AppStatus | undefined }) {
  return (
    <Card>
      <CardHeader title="Services" />
      <CardBody>
        {status ? (
          <dl>
            <KeyValue label="DNS">
              <StatusBadge spec={onOffSpec(status.dns.enabled)} />
            </KeyValue>
            <KeyValue label="Cache IPs">
              <Chips items={status.dns.cache_ips} />
            </KeyValue>
            <KeyValue label="Forwarders">
              <Chips items={status.dns.forwarders} />
            </KeyValue>
            <KeyValue label="SNI">
              <StatusBadge spec={onOffSpec(status.sni_enabled)} />
            </KeyValue>
            <KeyValue label="Uptime">{formatDuration(status.uptime_secs)}</KeyValue>
          </dl>
        ) : (
          <p className="text-sm text-text-muted">Loading...</p>
        )}
      </CardBody>
    </Card>
  );
}

function Chips({ items }: { items: string[] }) {
  if (items.length === 0) return <span className="text-text-muted">—</span>;
  return (
    <span className="flex flex-wrap gap-1">
      {items.map((i) => (
        <Badge key={i} variant="secondary">
          {i}
        </Badge>
      ))}
    </span>
  );
}

function PlatformTable({ platforms, loading }: { platforms: PlatformSummary[] | undefined; loading: boolean }) {
  return (
    <section className="mt-6">
      <h2 className="text-lg font-semibold mb-2">Platforms</h2>
      <Table>
        <THead>
          <tr>
            <Th>Platform</Th>
            <Th>State</Th>
            <Th className="text-right">Requests</Th>
            <Th className="text-right">Hit ratio</Th>
            <Th className="text-right">From cache</Th>
            <Th className="text-right">From upstream</Th>
            <Th className="text-right">Storage</Th>
          </tr>
        </THead>
        <tbody>
          {loading && !platforms && <SkeletonRows cols={7} />}
          {platforms?.map((p) => {
            const ratio = hitRatio(p.stats.hits, p.stats.misses);
            return (
              <Tr key={p.id}>
                <Td>
                  <Link to={`/platforms/${encodeURIComponent(p.id)}`} className="text-accent hover:text-accent-hover font-medium">
                    {p.name}
                  </Link>
                </Td>
                <Td>
                  <StatusBadge spec={enabledSpec(p.enabled)} />
                </Td>
                <Td className="text-right tabular-nums">{formatInt(p.stats.requests)}</Td>
                <Td className={cn("text-right tabular-nums", toneText(hitRatioTone(ratio)))}>{formatRatio(ratio)}</Td>
                <Td className="text-right tabular-nums">{formatBytes(p.stats.bytes_from_cache)}</Td>
                <Td className="text-right tabular-nums">{formatBytes(p.stats.bytes_from_upstream)}</Td>
                <Td className="text-right tabular-nums">{formatBytes(p.stats.storage_bytes)}</Td>
              </Tr>
            );
          })}
        </tbody>
      </Table>
    </section>
  );
}
