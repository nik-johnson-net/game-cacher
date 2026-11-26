import { useCallback, useState } from "react";
import { useParams } from "react-router";
import { Trash2 } from "lucide-react";
import { ApiError, getPlatform, purgePlatformCache } from "@/api/client";
import { ManagedSection } from "@/components/managed/managed-section";
import { EpicClientSetup } from "@/components/platforms/epic-client-setup";
import { KeyValue, Stat } from "@/components/stat";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { ConfirmDeleteDialog } from "@/components/ui/confirm-delete-dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Loading } from "@/components/ui/loading";
import { PageHeader } from "@/components/ui/page-header";
import { StatusBadge, toneText } from "@/components/ui/status-badge";
import { useToast } from "@/context/toast";
import { usePolling } from "@/hooks/usePolling";
import { formatBytes, formatInt, formatRatio, hitRatio } from "@/lib/format";
import { enabledSpec, hitRatioTone } from "@/lib/status";

export default function PlatformDetail() {
  const { id = "" } = useParams();
  const toast = useToast();
  const load = useCallback((signal: AbortSignal) => getPlatform(id, signal), [id]);
  const { data: p, error, loading, refresh } = usePolling(load, 5000);
  const [purgeOpen, setPurgeOpen] = useState(false);

  const crumbs = [{ label: "Platforms", href: "/platforms" }, { label: p?.name ?? id }];

  if (error && !p) {
    if (error instanceof ApiError && error.status === 404) {
      return (
        <div>
          <PageHeader breadcrumbs={crumbs} title={id} />
          <EmptyState title="Platform not found" description={error.message} />
        </div>
      );
    }
    return (
      <div>
        <PageHeader breadcrumbs={crumbs} title={id} />
        <AlertBanner variant="error" message={error.message} onRetry={refresh} />
      </div>
    );
  }
  if (!p) return loading ? <Loading /> : null;

  const ratio = hitRatio(p.stats.hits, p.stats.misses);

  const purge = async () => {
    const res = await purgePlatformCache(p.id);
    toast.push({
      variant: "success",
      title: `${p.name} cache purged`,
      message: `Freed ${formatBytes(res.freed_bytes)} (${formatInt(res.deleted_objects)} objects).`,
    });
    refresh();
  };

  return (
    <div>
      <PageHeader
        breadcrumbs={crumbs}
        title={p.name}
        description={p.description}
        status={
          <>
            <StatusBadge spec={enabledSpec(p.enabled)} />
            {p.managed && <Badge>managed updates</Badge>}
          </>
        }
        actions={
          <Button variant="danger" size="sm" onClick={() => setPurgeOpen(true)}>
            <Trash2 />
            Purge cache
          </Button>
        }
      />
      {error && <AlertBanner variant="error" message={error.message} onRetry={refresh} className="mb-4" />}

      <div className="grid gap-4 grid-cols-2 lg:grid-cols-4">
        <Stat label="Requests" value={formatInt(p.stats.requests)} hint={`${formatInt(p.stats.bypass)} bypassed`} />
        <Stat label="Hit ratio" value={<span className={toneText(hitRatioTone(ratio))}>{formatRatio(ratio)}</span>} hint={`${formatInt(p.stats.hits)} hits, ${formatInt(p.stats.misses)} misses`} />
        <Stat label="From cache" value={formatBytes(p.stats.bytes_from_cache)} hint={`${formatBytes(p.stats.bytes_from_upstream)} from upstream`} />
        <Stat label="Storage" value={formatBytes(p.stats.storage_bytes)} hint={`${formatInt(p.stats.objects)} objects`} />
      </div>

      <Card className="mt-6">
        <CardHeader title="Domains" description="Hostnames intercepted for this platform." />
        <CardBody>
          {p.domains.length === 0 ? (
            <p className="text-sm text-text-muted">No domains.</p>
          ) : (
            <dl>
              <KeyValue label={`${p.domains.length} domains`}>
                <span className="flex flex-wrap gap-1">
                  {p.domains.map((d) => (
                    <Badge key={d} variant="secondary">
                      {d}
                    </Badge>
                  ))}
                </span>
              </KeyValue>
            </dl>
          )}
        </CardBody>
      </Card>

      {p.id === "epic" && <EpicClientSetup />}

      {p.managed && <ManagedSection platformId={p.id} />}

      <ConfirmDeleteDialog
        open={purgeOpen}
        onOpenChange={setPurgeOpen}
        title="Purge cache"
        description="This deletes every cached object for this platform. Clients will re-download from upstream."
        itemName={p.name}
        confirmLabel="Purge cache"
        busyLabel="Purging..."
        onConfirm={purge}
      />
    </div>
  );
}
