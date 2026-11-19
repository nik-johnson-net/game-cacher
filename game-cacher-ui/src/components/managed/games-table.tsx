import { useCallback, useEffect, useMemo, useState } from "react";
import { Save } from "lucide-react";
import { errorMessage, listManagedGames, saveSelection } from "@/api/client";
import type { ManagedStatus } from "@/api/types";
import { AlertBanner } from "@/components/ui/alert-banner";
import { Button } from "@/components/ui/button";
import { Card, CardHeader } from "@/components/ui/card";
import { Input } from "@/components/ui/form";
import { SkeletonRows, Spinner } from "@/components/ui/loading";
import { Table, THead, Th, Tr, Td, TableMessageRow } from "@/components/ui/table";
import { useToast } from "@/context/toast";
import { usePolling } from "@/hooks/usePolling";
import { formatBytes } from "@/lib/format";

interface GamesTableProps {
  platformId: string;
  /** Saved selection (from ManagedStatus), the baseline for "unsaved changes". */
  saved: string[];
  /** Changes whenever the account changes, to reload the library. */
  accountKey: string;
  onSaved: (status: ManagedStatus) => void;
}

function sameSet(a: Set<string>, b: string[]): boolean {
  return a.size === b.length && b.every((x) => a.has(x));
}

export function GamesTable({ platformId, saved, accountKey, onSaved }: GamesTableProps) {
  const toast = useToast();
  const load = useCallback(
    (signal: AbortSignal) => {
      void accountKey; // refetch after sign-in / sign-out
      return listManagedGames(platformId, signal);
    },
    [platformId, accountKey],
  );
  const { data: games, error, loading, refresh } = usePolling(load, 0);
  const [selected, setSelected] = useState<Set<string>>(() => new Set(saved));
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  // Adopt the server's per-game `selected` flags whenever the list is (re)loaded.
  useEffect(() => {
    if (games) setSelected(new Set(games.filter((g) => g.selected).map((g) => g.id)));
  }, [games]);

  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return (games ?? []).filter((g) => !q || g.name.toLowerCase().includes(q) || g.id.toLowerCase().includes(q));
  }, [games, filter]);

  const dirty = !sameSet(selected, saved);

  const toggle = (id: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  const allVisibleSelected = visible.length > 0 && visible.every((g) => selected.has(g.id));
  const toggleVisible = (on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      for (const g of visible) {
        if (on) next.add(g.id);
        else next.delete(g.id);
      }
      return next;
    });

  const save = async () => {
    setBusy(true);
    setSaveError(null);
    try {
      const status = await saveSelection(platformId, [...selected]);
      onSaved(status);
      toast.push({ variant: "success", message: `Selection saved (${selected.size} games).` });
    } catch (e) {
      setSaveError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHeader
        title="Games"
        description={games ? `${selected.size} of ${games.length} selected for managed updates` : undefined}
        actions={
          <>
            <Input
              aria-label="Filter games"
              placeholder="Filter by name or id"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              className="w-56 h-7"
            />
            <Button size="sm" onClick={() => void save()} disabled={busy || !dirty}>
              {busy ? <Spinner /> : <Save />}
              Save selection
            </Button>
          </>
        }
      />
      {error && <AlertBanner variant="error" message={error.message} onRetry={refresh} />}
      <AlertBanner variant="error" message={saveError} />
      <Table wrapperClassName="border-0 border-t-0 max-h-[480px] overflow-y-auto">
        <THead className="sticky top-0">
          <tr>
            <Th className="w-8">
              <input
                type="checkbox"
                aria-label="Select all shown games"
                checked={allVisibleSelected}
                disabled={visible.length === 0}
                onChange={(e) => toggleVisible(e.target.checked)}
              />
            </Th>
            <Th>Name</Th>
            <Th>Id</Th>
            <Th>Version</Th>
            <Th className="text-right">Size</Th>
          </tr>
        </THead>
        <tbody>
          {loading && !games && <SkeletonRows cols={5} />}
          {games && visible.length === 0 && (
            <TableMessageRow colSpan={5}>
              <p className="p-4 text-sm text-text-secondary">{games.length === 0 ? "No games found for this account." : "No games match the filter."}</p>
            </TableMessageRow>
          )}
          {visible.map((g) => (
            <Tr key={g.id}>
              <Td>
                <input
                  type="checkbox"
                  aria-label={`Select ${g.name}`}
                  checked={selected.has(g.id)}
                  onChange={(e) => toggle(g.id, e.target.checked)}
                />
              </Td>
              <Td>{g.name}</Td>
              <Td className="text-text-secondary">{g.id}</Td>
              <Td className="text-text-secondary">{g.version ?? "—"}</Td>
              <Td className="text-right tabular-nums">{formatBytes(g.size_bytes)}</Td>
            </Tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}
