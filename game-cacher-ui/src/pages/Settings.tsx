import { useCallback } from "react";
import { getSettings } from "@/api/client";
import { ScheduleCard } from "@/components/settings/schedule-card";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Loading } from "@/components/ui/loading";
import { PageHeader } from "@/components/ui/page-header";
import { usePolling } from "@/hooks/usePolling";

export default function SettingsPage() {
  const load = useCallback((signal: AbortSignal) => getSettings(signal), []);
  const { data, error, loading, refresh, setData } = usePolling(load, 0);

  return (
    <div>
      <PageHeader title="Settings" description="Runtime settings stored by the app." />
      {error && !data && <AlertBanner variant="error" message={error.message} onRetry={refresh} />}
      {loading && !data && <Loading />}
      {data && (
        <ScheduleCard key={JSON.stringify(data.update_schedule)} initial={data.update_schedule} onSaved={setData} />
      )}

      <Card className="mt-6 max-w-xl">
        <CardHeader title="Infrastructure" description="Read-only" />
        <CardBody className="text-sm text-text-secondary space-y-2">
          <p>
            Storage (disk or S3, size limits), secrets (storage or Vault), DNS and SNI listeners are configured through
            environment variables and command-line flags when game-cacher starts. They cannot be changed from the UI;
            restart the container with new values to apply them.
          </p>
          <p>
            The Dashboard shows the values currently in effect.
          </p>
        </CardBody>
      </Card>
    </div>
  );
}
