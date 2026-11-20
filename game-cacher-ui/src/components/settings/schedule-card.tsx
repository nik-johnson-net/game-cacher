import { useMemo, useState, type FormEvent } from "react";
import { Plus, X } from "lucide-react";
import { errorMessage, putSettings } from "@/api/client";
import type { Settings, UpdateSchedule } from "@/api/types";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Field, FieldError, Input, Label, Select } from "@/components/ui/form";
import { Spinner } from "@/components/ui/loading";
import { useToast } from "@/context/toast";

const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/;
const BROWSER_TZ = Intl.DateTimeFormat().resolvedOptions().timeZone;

/** Sorted, de-duplicated — the form the server stores. */
function normalize(times: string[]): string[] {
  return [...new Set(times)].sort();
}

/** One hour after the latest time, wrapping at midnight, skipping times already listed. */
function suggestTime(times: string[]): string {
  const valid = times.filter((t) => TIME_RE.test(t)).sort();
  const start = valid.length ? Number(valid[valid.length - 1].slice(0, 2)) + 1 : 3;
  for (let i = 0; i < 24; i++) {
    const t = `${String((start + i) % 24).padStart(2, "0")}:00`;
    if (!valid.includes(t)) return t;
  }
  return "00:00";
}

function validate(times: string[]): string | null {
  if (times.some((t) => !TIME_RE.test(t))) return "Enter every time as HH:MM.";
  if (new Set(times).size !== times.length) return "Each time can only be listed once.";
  return null;
}

export function ScheduleCard({ initial, onSaved }: { initial: UpdateSchedule; onSaved: (s: Settings) => void }) {
  const toast = useToast();
  const [times, setTimes] = useState(initial.times);
  const [timezone, setTimezone] = useState(initial.timezone);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const zones = useMemo(() => {
    // Browsers may report a legacy alias (e.g. Asia/Calcutta) missing from the list; the server accepts aliases.
    return [...new Set(["UTC", ...Intl.supportedValuesOf("timeZone"), initial.timezone, BROWSER_TZ])];
  }, [initial.timezone]);

  const invalid = validate(times);
  const dirty = timezone !== initial.timezone || normalize(times).join() !== initial.times.join();

  const setTime = (i: number, value: string) => setTimes((ts) => ts.map((t, j) => (j === i ? value : t)));
  const removeTime = (i: number) => setTimes((ts) => ts.filter((_, j) => j !== i));
  const addTime = () => setTimes((ts) => [...ts, suggestTime(ts)]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (invalid) return;
    setBusy(true);
    setErr(null);
    try {
      const saved = await putSettings({ update_schedule: { timezone, times: normalize(times) } });
      onSaved(saved);
      const { times: st, timezone: tz } = saved.update_schedule;
      toast.push({
        variant: "success",
        message: st.length ? `Updates run daily at ${st.join(", ")} (${tz}).` : "Scheduled updates are off; use Run now.",
      });
    } catch (e2) {
      setErr(errorMessage(e2));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="max-w-xl">
      <CardHeader title="Managed updates" description="When selected games are checked for new versions, every day." />
      <form onSubmit={submit}>
        <CardBody className="space-y-4">
          <AlertBanner variant="error" message={err} />
          <div>
            <Label>Run at</Label>
            {times.length === 0 ? (
              <p className="text-sm text-text-secondary py-1">No times set — updates only run when you click Run now.</p>
            ) : (
              <ul className="flex flex-wrap gap-2">
                {times.map((t, i) => (
                  <li key={i} className="flex items-center gap-1">
                    <Input
                      type="time"
                      step={60}
                      required
                      value={t}
                      onChange={(e) => setTime(i, e.target.value)}
                      aria-label={`Update time ${i + 1}`}
                      aria-invalid={!TIME_RE.test(t) || times.indexOf(t) !== i || undefined}
                      className="w-28 tabular-nums"
                    />
                    <Button variant="ghost-danger" size="icon-sm" onClick={() => removeTime(i)} aria-label={`Remove ${t || "time"}`}>
                      <X />
                    </Button>
                  </li>
                ))}
              </ul>
            )}
            <FieldError error={invalid} />
            <Button variant="secondary" size="sm" className="mt-2" onClick={addTime}>
              <Plus />
              Add time
            </Button>
          </div>
          <Field
            label="Timezone"
            htmlFor="timezone"
            hint={
              timezone === BROWSER_TZ ? (
                "Applies to every platform with managed updates. Daylight saving is followed."
              ) : (
                <>
                  Your browser is in {BROWSER_TZ}.{" "}
                  <Button variant="link" className="text-xs" onClick={() => setTimezone(BROWSER_TZ)}>
                    Use it
                  </Button>
                </>
              )
            }
          >
            <Select id="timezone" value={timezone} onChange={(e) => setTimezone(e.target.value)} className="w-72">
              {zones.map((z) => (
                <option key={z} value={z}>
                  {z}
                </option>
              ))}
            </Select>
          </Field>
          <Button type="submit" disabled={busy || !!invalid || !dirty}>
            {busy && <Spinner />}
            Save
          </Button>
        </CardBody>
      </form>
    </Card>
  );
}
