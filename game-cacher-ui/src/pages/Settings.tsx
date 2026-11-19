import { useCallback, useState, type FormEvent } from "react";
import { errorMessage, getSettings, putSettings } from "@/api/client";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Field, Input } from "@/components/ui/form";
import { Loading, Spinner } from "@/components/ui/loading";
import { PageHeader } from "@/components/ui/page-header";
import { useToast } from "@/context/toast";
import { usePolling } from "@/hooks/usePolling";

export default function SettingsPage() {
  const load = useCallback((signal: AbortSignal) => getSettings(signal), []);
  const { data, error, loading, refresh, setData } = usePolling(load, 0);

  return (
    <div>
      <PageHeader title="Settings" description="Runtime settings stored by the app." />
      {error && !data && <AlertBanner variant="error" message={error.message} onRetry={refresh} />}
      {loading && !data && <Loading />}
      {data && <IntervalCard key={data.update_interval_minutes} initial={data.update_interval_minutes} onSaved={setData} />}

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

function IntervalCard({ initial, onSaved }: { initial: number; onSaved: (s: { update_interval_minutes: number }) => void }) {
  const toast = useToast();
  const [value, setValue] = useState(String(initial));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const minutes = Number(value);
  const valid = Number.isInteger(minutes) && minutes >= 1;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!valid) return;
    setBusy(true);
    setErr(null);
    try {
      const saved = await putSettings({ update_interval_minutes: minutes });
      onSaved(saved);
      toast.push({ variant: "success", message: `Update interval set to ${saved.update_interval_minutes} minutes.` });
    } catch (e2) {
      setErr(errorMessage(e2));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="max-w-xl">
      <CardHeader title="Managed updates" description="How often selected games are checked for new versions." />
      <form onSubmit={submit}>
        <CardBody className="space-y-4">
          <AlertBanner variant="error" message={err} />
          <Field
            label="Update interval (minutes)"
            htmlFor="interval"
            hint="Applies to every platform with managed updates."
            error={value !== "" && !valid ? "Enter a whole number of minutes, 1 or more." : null}
          >
            <Input id="interval" type="number" min={1} step={1} value={value} onChange={(e) => setValue(e.target.value)} className="w-40" />
          </Field>
          <Button type="submit" disabled={busy || !valid || minutes === initial}>
            {busy && <Spinner />}
            Save
          </Button>
        </CardBody>
      </form>
    </Card>
  );
}
