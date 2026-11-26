import type { ReactNode } from "react";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { CopyButton } from "@/components/ui/copy-button";

const SETTINGS_PATH = String.raw`%LOCALAPPDATA%\EpicGamesLauncher\Saved\Config\WindowsEditor\GameUserSettings.ini`;
const SETTINGS_SNIPPET = "[Launcher]\nUseHttpForDownloads=True";

/** Monospace block with a copy button, for paths and config snippets. */
function CodeBlock({ value, label }: { value: string; label: string }) {
  return (
    <div className="flex items-start gap-2 mt-1.5 bg-bg-base border border-border px-3 py-2">
      <pre className="flex-1 min-w-0 text-sm text-text-primary whitespace-pre-wrap break-all">{value}</pre>
      <CopyButton value={value} label={label} />
    </div>
  );
}

function Step({ n, children }: { n: number; children: ReactNode }) {
  return (
    <li className="flex gap-3">
      <span className="text-xs font-semibold text-accent tabular-nums pt-px w-4 shrink-0">{n}.</span>
      <div className="flex-1 min-w-0 text-sm text-text-primary">{children}</div>
    </li>
  );
}

const code = "text-text-primary bg-bg-raised px-1 rounded-sm";

/** How to switch Epic Games Launcher clients to plain HTTP downloads so they hit the cache. */
export function EpicClientSetup() {
  return (
    <Card className="mt-6">
      <CardHeader
        title="Client setup: enable HTTP downloads"
        description="The Epic Games Launcher downloads over HTTPS by default. game-cacher passes HTTPS through untouched, so those downloads are never cached. Switch each Windows client to HTTP once."
      />
      <CardBody className="space-y-4">
        <ol className="space-y-3">
          <Step n={1}>
            Fully exit the Epic Games Launcher (system tray icon → <span className="font-semibold">Exit</span>) so it does not
            overwrite your change.
          </Step>
          <Step n={2}>
            Open this file for the Windows user that runs the launcher (create it if it is missing):
            <CodeBlock value={SETTINGS_PATH} label="Copy path" />
          </Step>
          <Step n={3}>
            Add these lines. If the file already has a <code className={code}>[Launcher]</code> section, add only the second line
            under it.
            <CodeBlock value={SETTINGS_SNIPPET} label="Copy settings" />
          </Step>
          <Step n={4}>
            Start the launcher and download or update a game. This page's <span className="font-semibold">Requests</span> and{" "}
            <span className="font-semibold">From cache</span> should rise; a second client downloading the same game is served from
            the cache.
          </Step>
        </ol>
        <ul className="space-y-1 text-xs text-text-secondary list-disc pl-4">
          <li>
            The setting is per Windows user and is kept by the launcher. Newer launchers may show it as a download option in Settings.
          </li>
          <li>
            The launcher passes it to the Epic Online Services helper that runs game installs, which then requests chunks over
            plain HTTP.
          </li>
          <li>Clients without it still download normally, they just bypass the cache. Managed updates are not affected.</li>
        </ul>
      </CardBody>
    </Card>
  );
}
