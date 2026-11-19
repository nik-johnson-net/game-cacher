import { useState } from "react";
import { Save } from "lucide-react";
import { errorMessage, saveSelection } from "@/api/client";
import type { ManagedStatus } from "@/api/types";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Textarea } from "@/components/ui/form";
import { Spinner } from "@/components/ui/loading";
import { useToast } from "@/context/toast";

function parseLines(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
}

/** Manual URL list (Windows Update): one URL per line, saved as the selection. */
export function ManualList({ platformId, saved, onSaved }: { platformId: string; saved: string[]; onSaved: (s: ManagedStatus) => void }) {
  const toast = useToast();
  const [text, setText] = useState(saved.join("\n"));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const lines = parseLines(text);
  const dirty = lines.join("\n") !== saved.join("\n");

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      onSaved(await saveSelection(platformId, lines));
      toast.push({ variant: "success", message: `Saved ${lines.length} URLs.` });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHeader
        title="Update URLs"
        description="One URL per line. These files are kept fresh in the cache on every run."
        actions={
          <Button size="sm" onClick={() => void save()} disabled={busy || !dirty}>
            {busy ? <Spinner /> : <Save />}
            Save selection
          </Button>
        }
      />
      <CardBody className="space-y-3">
        <AlertBanner variant="error" message={error} />
        <Textarea
          aria-label="Update URLs"
          rows={8}
          spellCheck={false}
          placeholder="https://download.windowsupdate.com/..."
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <div className="text-xs text-text-muted">{lines.length} URLs</div>
      </CardBody>
    </Card>
  );
}
