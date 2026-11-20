import { useState } from "react";
import { Square } from "lucide-react";
import { ApiError, cancelRun, errorMessage } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/loading";
import { useToast } from "@/context/toast";

/** Asks the platform's active run to stop (it stops after dropping in-flight downloads). */
export function CancelRunButton({ platformId, onChanged }: { platformId: string; onChanged: () => void }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);

  const cancel = async () => {
    setBusy(true);
    try {
      await cancelRun(platformId);
      toast.push({ variant: "info", message: "Cancelling the update run." });
    } catch (e) {
      // 409: the run already finished.
      if (!(e instanceof ApiError && e.status === 409)) {
        toast.push({ variant: "error", title: "Could not cancel run", message: errorMessage(e) });
      }
    } finally {
      setBusy(false);
      onChanged();
    }
  };

  return (
    <Button size="sm" variant="danger" onClick={() => void cancel()} disabled={busy}>
      {busy ? <Spinner /> : <Square />}
      Cancel
    </Button>
  );
}
