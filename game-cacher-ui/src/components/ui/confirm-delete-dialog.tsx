import { useState, type ReactNode } from "react";
import { errorMessage } from "@/api/client";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "./alert-dialog";
import { AlertBanner } from "./alert-banner";
import { Button } from "./button";
import { Spinner } from "./loading";

interface ConfirmDeleteDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: ReactNode;
  /** Item being deleted, shown emphasized under the description. */
  itemName?: string;
  confirmLabel?: string;
  busyLabel?: string;
  onConfirm: () => Promise<void>;
}

/** Controlled delete confirmation. Content (and its state) unmounts when closed. */
export function ConfirmDeleteDialog(props: ConfirmDeleteDialogProps) {
  return (
    <AlertDialog open={props.open} onOpenChange={props.onOpenChange}>
      <AlertDialogContent>
        <DialogBody {...props} />
      </AlertDialogContent>
    </AlertDialog>
  );
}

function DialogBody({
  onOpenChange,
  title,
  description,
  itemName,
  confirmLabel = "Delete",
  busyLabel = "Deleting...",
  onConfirm,
}: ConfirmDeleteDialogProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      onOpenChange(false);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>{title}</AlertDialogTitle>
        <AlertDialogDescription asChild>
          <div>
            {description}
            {itemName && <strong className="block mt-2 text-text-primary font-medium break-all">{itemName}</strong>}
          </div>
        </AlertDialogDescription>
      </AlertDialogHeader>

      <AlertBanner variant="error" message={error} />

      <AlertDialogFooter>
        <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
        <Button variant="danger" onClick={() => run()} disabled={busy}>
          {busy && <Spinner />}
          {busy ? busyLabel : confirmLabel}
        </Button>
      </AlertDialogFooter>
    </>
  );
}
