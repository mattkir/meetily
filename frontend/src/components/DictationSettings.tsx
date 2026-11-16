'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Switch } from './ui/switch';

type DictationEngine = 'apple' | 'parakeet_v3_coreml';
type PolishMode = 'heuristic' | 'always_llm' | 'long_only' | 'off';

type DictationSettings = {
  enabled: boolean;
  holdShortcut: string;
  handsFreeShortcut: string;
  engine: DictationEngine;
  polishMode: PolishMode;
  polishModel?: string | null;
  dictionary: Record<string, string>;
  snippets: Record<string, string>;
  allowDuringMeeting: boolean;
  launchAtLogin?: boolean;
  handsFreeSilenceMs?: number;
  handsFreeMaxMs?: number;
};

type DictationPermissions = {
  microphone: boolean;
  accessibility: boolean;
  inputMonitoring: boolean;
};

type LatencyMetrics = {
  graceMs: number;
  resampleMs: number;
  ipcMs: number;
  asrMs: number;
  polishMs: number;
  insertMs: number;
  e2eMs: number;
};

type DictationStatus = {
  phase: string;
  message: string;
  enabled: boolean;
  helperReady: boolean;
  warmReady?: boolean;
  dailyReady?: boolean;
  handsFree: boolean;
  lastMetrics?: LatencyMetrics | null;
};

type SelfTestResult = {
  ok: boolean;
  dailyReady: boolean;
  message: string;
  metrics: LatencyMetrics;
};

type HistoryEntry = {
  id: string;
  createdAt: string;
  text: string;
  engine: string;
  durationMs: number;
  insertMethod: string;
};

const DEFAULT_SETTINGS: DictationSettings = {
  enabled: true,
  holdShortcut: 'fn',
  handsFreeShortcut: 'ctrl+shift+space',
  engine: 'apple',
  polishMode: 'heuristic',
  polishModel: null,
  dictionary: {},
  snippets: {},
  allowDuringMeeting: false,
  launchAtLogin: false,
  handsFreeSilenceMs: 1200,
  handsFreeMaxMs: 60000,
};

function parseArrowMap(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of raw.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const sep = trimmed.includes('→') ? '→' : trimmed.includes('->') ? '->' : null;
    if (!sep) continue;
    const [from, to] = trimmed.split(sep).map((p) => p.trim());
    if (from) out[from] = to || '';
  }
  return out;
}

function formatArrowMap(map: Record<string, string> | undefined): string {
  return Object.entries(map || {})
    .map(([k, v]) => `${k} → ${v}`)
    .join('\n');
}

function formatHistoryTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function formatMetrics(m: LatencyMetrics): string {
  return [
    `grace ${m.graceMs}ms`,
    `resample ${m.resampleMs}ms`,
    `ipc ${m.ipcMs}ms`,
    `asr ${m.asrMs}ms`,
    `polish ${m.polishMs}ms`,
    `insert ${m.insertMs}ms`,
    `e2e ${m.e2eMs}ms`,
  ].join(' · ');
}

export function DictationSettings() {
  const [settings, setSettings] = useState<DictationSettings>(DEFAULT_SETTINGS);
  const [perms, setPerms] = useState<DictationPermissions | null>(null);
  const [status, setStatus] = useState<DictationStatus | null>(null);
  const [helperAvailable, setHelperAvailable] = useState(false);
  const [dictDraft, setDictDraft] = useState('');
  const [snippetsDraft, setSnippetsDraft] = useState('');
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [saveState, setSaveState] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [selfTestResult, setSelfTestResult] = useState<string | null>(null);
  const [selfTestBusy, setSelfTestBusy] = useState(false);
  const [historyAction, setHistoryAction] = useState<string | null>(null);

  const permissionsOk =
    !!perms?.microphone && !!perms?.accessibility && !!perms?.inputMonitoring;
  const readyOk = !!status?.warmReady && !!status?.helperReady && helperAvailable;
  const hardBlocked = !permissionsOk || !helperAvailable;

  const refresh = useCallback(async () => {
    try {
      const [s, p, st, helper, hist] = await Promise.all([
        invoke<DictationSettings>('dictation_get_settings'),
        invoke<DictationPermissions>('dictation_get_permissions'),
        invoke<DictationStatus>('dictation_get_status'),
        invoke<boolean>('dictation_helper_available'),
        invoke<HistoryEntry[]>('dictation_get_history'),
      ]);
      setSettings({
        ...DEFAULT_SETTINGS,
        ...s,
        dictionary: s.dictionary || {},
        snippets: s.snippets || {},
      });
      setPerms(p);
      setStatus(st);
      setHelperAvailable(helper);
      setHistory(hist || []);
      setDictDraft(formatArrowMap(s.dictionary));
      setSnippetsDraft(formatArrowMap(s.snippets));
    } catch (e) {
      console.error('Failed to load dictation settings', e);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const save = async (next: DictationSettings) => {
    try {
      await invoke('dictation_set_settings', { settings: next });
      setSettings(next);
      setSaveState('Saved');
      setTimeout(() => setSaveState(null), 1500);
      await refresh();
    } catch (e) {
      setSaveState(`Error: ${e}`);
    }
  };

  const openPane = async (pane: string) => {
    await invoke('dictation_open_permission_settings', { pane });
  };

  return (
    <div className="space-y-8 max-w-2xl">
      <div>
        <h2 className="text-xl font-semibold mb-1">Dictation</h2>
        <p className="text-sm text-gray-600">
          Hold Fn to dictate into any text field. Works with the main window hidden.
          Short utterances use heuristic cleanup only; LLM polish is opt-in.
        </p>
      </div>

      <section className="space-y-3 rounded-lg border border-gray-200 p-4">
        <div className="flex items-center justify-between">
          <div>
            <div className="font-medium">Enable dictation agent</div>
            <div className="text-xs text-gray-500">
              Status: {status?.phase ?? '…'}
              {status?.message ? ` — ${status.message}` : ''}
            </div>
            <div className="text-xs mt-1">
              {status?.dailyReady ? (
                <span className="text-emerald-700">Daily ready</span>
              ) : readyOk ? (
                <span className="text-amber-700">Warmed — run self-test for daily-ready</span>
              ) : (
                <span className="text-rose-700">Not ready</span>
              )}
              {hardBlocked ? (
                <span className="text-rose-700"> · blocked until permissions + helper</span>
              ) : null}
            </div>
          </div>
          <Switch
            checked={settings.enabled}
            disabled={hardBlocked}
            onCheckedChange={(enabled) => save({ ...settings, enabled })}
          />
        </div>

        <div className="flex items-center justify-between">
          <div>
            <div className="font-medium">Launch at login</div>
            <div className="text-xs text-gray-500">
              Starts hidden to tray (`--hidden` LaunchAgent)
            </div>
          </div>
          <Switch
            checked={!!settings.launchAtLogin}
            onCheckedChange={(launchAtLogin) => save({ ...settings, launchAtLogin })}
          />
        </div>

        <div className="flex items-center justify-between">
          <div>
            <div className="font-medium">Allow during meeting recording</div>
            <div className="text-xs text-gray-500">
              Default off to protect meeting mic capture
            </div>
          </div>
          <Switch
            checked={settings.allowDuringMeeting}
            onCheckedChange={(allowDuringMeeting) =>
              save({ ...settings, allowDuringMeeting })
            }
          />
        </div>
      </section>

      <section
        className={`space-y-3 rounded-lg border p-4 ${
          permissionsOk ? 'border-gray-200' : 'border-rose-300 bg-rose-50/40'
        }`}
      >
        <h3 className="font-medium">Permissions (required)</h3>
        {!permissionsOk ? (
          <p className="text-xs text-rose-700">
            Dictation stays blocked until Microphone, Accessibility, and Input Monitoring
            are granted.
          </p>
        ) : null}
        <PermissionRow
          label="Microphone"
          granted={perms?.microphone}
          onGrant={async () => {
            await invoke('dictation_trigger_microphone_permission');
            await openPane('microphone');
            await refresh();
          }}
          onOpen={() => openPane('microphone')}
        />
        <PermissionRow
          label="Accessibility"
          granted={perms?.accessibility}
          onGrant={async () => {
            await invoke('dictation_request_accessibility');
            await openPane('accessibility');
            await refresh();
          }}
          onOpen={() => openPane('accessibility')}
        />
        <PermissionRow
          label="Input Monitoring"
          granted={perms?.inputMonitoring}
          onGrant={() => openPane('input_monitoring')}
          onOpen={() => openPane('input_monitoring')}
        />
        {!helperAvailable ? (
          <p className="text-xs text-rose-700">
            dictation-helper is missing — insert is refused (fail-closed). Stage the Swift
            sidecar under `src-tauri/binaries/`.
          </p>
        ) : null}
      </section>

      <section className="space-y-3 rounded-lg border border-gray-200 p-4">
        <h3 className="font-medium">Shortcuts</h3>
        <label className="block text-sm">
          <span className="text-gray-600">Hold-to-talk</span>
          <input
            className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm"
            value={settings.holdShortcut}
            onChange={(e) => setSettings({ ...settings, holdShortcut: e.target.value })}
            onBlur={() => save(settings)}
          />
          <span className="text-xs text-gray-500">
            `fn` (default) or a chord like `ctrl+shift+d`. Binding is live after save.
          </span>
        </label>
        <label className="block text-sm">
          <span className="text-gray-600">Hands-free toggle</span>
          <input
            className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm"
            value={settings.handsFreeShortcut}
            onChange={(e) =>
              setSettings({ ...settings, handsFreeShortcut: e.target.value })
            }
            onBlur={() => save(settings)}
          />
          <span className="text-xs text-gray-500">
            Default `ctrl+shift+space`. Ends on silence or max duration; Esc cancels.
          </span>
        </label>
      </section>

      <section className="space-y-3 rounded-lg border border-gray-200 p-4">
        <h3 className="font-medium">Engine &amp; polish</h3>
        <label className="block text-sm">
          <span className="text-gray-600">ASR engine</span>
          <select
            className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm"
            value={settings.engine}
            onChange={(e) =>
              save({
                ...settings,
                engine: e.target.value as DictationEngine,
              })
            }
          >
            <option value="apple">Apple SpeechTranscriber (default, live partials)</option>
            <option value="parakeet_v3_coreml">Parakeet v3 Core ML (final on release)</option>
          </select>
          <span className="text-xs text-gray-500">
            Helper: {helperAvailable ? 'available' : 'missing'} · warm:{' '}
            {status?.warmReady ? 'yes' : 'no'}
          </span>
        </label>
        <label className="block text-sm">
          <span className="text-gray-600">Polish mode</span>
          <select
            className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm"
            value={settings.polishMode}
            onChange={(e) =>
              save({
                ...settings,
                polishMode: e.target.value as PolishMode,
              })
            }
          >
            <option value="heuristic">Heuristic + dictionary (default, &lt;10ms)</option>
            <option value="long_only">LLM polish on long utterances (~9s+)</option>
            <option value="always_llm">Always polish with local LLM</option>
            <option value="off">Off (raw ASR text)</option>
          </select>
        </label>
        {(settings.polishMode === 'always_llm' || settings.polishMode === 'long_only') && (
          <label className="block text-sm">
            <span className="text-gray-600">Polish model (optional)</span>
            <input
              className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm font-mono"
              placeholder="qwen3.5:4b"
              value={settings.polishModel ?? ''}
              onChange={(e) =>
                setSettings({
                  ...settings,
                  polishModel: e.target.value || null,
                })
              }
              onBlur={() =>
                save({
                  ...settings,
                  polishModel: settings.polishModel?.trim() || null,
                })
              }
            />
          </label>
        )}
        <label className="block text-sm">
          <span className="text-gray-600">Personal dictionary (one per line: from → to)</span>
          <textarea
            className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm min-h-[96px] font-mono"
            value={dictDraft}
            onChange={(e) => setDictDraft(e.target.value)}
            onBlur={() =>
              save({
                ...settings,
                dictionary: parseArrowMap(dictDraft),
              })
            }
            placeholder={'meet ily → Meetily\nkirchoff → Kirchoff'}
          />
        </label>
        <label className="block text-sm">
          <span className="text-gray-600">Snippets (spoken trigger → expansion)</span>
          <textarea
            className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm min-h-[96px] font-mono"
            value={snippetsDraft}
            onChange={(e) => setSnippetsDraft(e.target.value)}
            onBlur={() =>
              save({
                ...settings,
                snippets: parseArrowMap(snippetsDraft),
              })
            }
            placeholder={'email sig → Best regards,\nMatt'}
          />
        </label>
      </section>

      <section className="space-y-3 rounded-lg border border-gray-200 p-4">
        <div className="flex items-center justify-between gap-3">
          <div>
            <h3 className="font-medium">Local history</h3>
            <p className="text-xs text-gray-500">
              Stored only on this Mac. Retry pastes again; Copy puts text on the clipboard.
            </p>
          </div>
          <button
            type="button"
            className="text-xs rounded border border-gray-300 px-2 py-1 hover:bg-gray-50 disabled:opacity-40"
            disabled={history.length === 0}
            onClick={async () => {
              await invoke('dictation_clear_history');
              setHistoryAction('Cleared');
              await refresh();
            }}
          >
            Clear all
          </button>
        </div>
        {history.length === 0 ? (
          <p className="text-sm text-gray-500">No dictations yet.</p>
        ) : (
          <ul className="space-y-3 max-h-72 overflow-y-auto">
            {history.map((entry) => (
              <li
                key={entry.id}
                className="rounded border border-gray-100 bg-gray-50 p-3 text-sm"
              >
                <div className="flex items-start justify-between gap-2 mb-1">
                  <div className="text-xs text-gray-500">
                    {formatHistoryTime(entry.createdAt)} · {entry.durationMs}ms ·{' '}
                    {entry.insertMethod}
                  </div>
                  <div className="flex shrink-0 gap-1">
                    <button
                      type="button"
                      className="text-xs rounded border border-gray-300 px-2 py-0.5 hover:bg-white"
                      onClick={async () => {
                        try {
                          const method = await invoke<string>(
                            'dictation_retry_history_insert',
                            { id: entry.id }
                          );
                          setHistoryAction(`Retried via ${method}`);
                        } catch (e) {
                          setHistoryAction(`Retry failed: ${e}`);
                        }
                      }}
                    >
                      Retry
                    </button>
                    <button
                      type="button"
                      className="text-xs rounded border border-gray-300 px-2 py-0.5 hover:bg-white"
                      onClick={async () => {
                        try {
                          await invoke('dictation_copy_history_entry', { id: entry.id });
                          setHistoryAction('Copied');
                        } catch (e) {
                          setHistoryAction(`Copy failed: ${e}`);
                        }
                      }}
                    >
                      Copy
                    </button>
                    <button
                      type="button"
                      className="text-xs rounded border border-gray-300 px-2 py-0.5 hover:bg-white text-red-700"
                      onClick={async () => {
                        await invoke('dictation_remove_history_entry', { id: entry.id });
                        await refresh();
                      }}
                    >
                      Delete
                    </button>
                  </div>
                </div>
                <p className="whitespace-pre-wrap break-words text-gray-800">{entry.text}</p>
              </li>
            ))}
          </ul>
        )}
        {historyAction ? (
          <p className="text-sm text-gray-500">{historyAction}</p>
        ) : null}
      </section>

      <section className="space-y-3 rounded-lg border border-gray-200 p-4">
        <h3 className="font-medium">Verify</h3>
        <p className="text-xs text-gray-500">
          Self-test warms ASR and reports latency buckets (no transcript text in logs).
          Daily-ready requires warm short e2e &lt; 300ms. Optional insert pastes a mid-dot
          into the focused field.
        </p>
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            className="rounded bg-blue-600 text-white px-3 py-2 text-sm hover:bg-blue-700 disabled:opacity-40"
            disabled={selfTestBusy || !helperAvailable}
            onClick={async () => {
              setSelfTestBusy(true);
              setSelfTestResult(null);
              try {
                const result = await invoke<SelfTestResult>('dictation_run_self_test', {
                  includeInsert: false,
                });
                setSelfTestResult(
                  `${result.message}\n${formatMetrics(result.metrics)}`
                );
                await refresh();
              } catch (e) {
                setSelfTestResult(`Self-test failed: ${e}`);
              } finally {
                setSelfTestBusy(false);
              }
            }}
          >
            {selfTestBusy ? 'Running…' : 'Self-test (ASR path)'}
          </button>
          <button
            type="button"
            className="rounded border border-gray-300 px-3 py-2 text-sm hover:bg-gray-50 disabled:opacity-40"
            disabled={selfTestBusy || !helperAvailable || !permissionsOk}
            onClick={async () => {
              setSelfTestBusy(true);
              setSelfTestResult(null);
              try {
                const result = await invoke<SelfTestResult>('dictation_run_self_test', {
                  includeInsert: true,
                });
                setSelfTestResult(
                  `${result.message}\n${formatMetrics(result.metrics)}`
                );
                await refresh();
              } catch (e) {
                setSelfTestResult(`Self-test failed: ${e}`);
              } finally {
                setSelfTestBusy(false);
              }
            }}
          >
            Self-test + insert
          </button>
          <button
            type="button"
            className="rounded border border-gray-300 px-3 py-2 text-sm hover:bg-gray-50"
            onClick={async () => {
              try {
                const method = await invoke<string>('dictation_test_insert', {
                  text: 'Meetily dictation paste OK',
                });
                setTestResult(`Inserted via ${method}`);
              } catch (e) {
                setTestResult(`Failed: ${e}`);
              }
            }}
          >
            Test paste
          </button>
          <button
            type="button"
            className="rounded border border-gray-300 px-3 py-2 text-sm hover:bg-gray-50"
            onClick={() => refresh()}
          >
            Refresh status
          </button>
        </div>
        {selfTestResult ? (
          <pre className="text-sm text-gray-700 whitespace-pre-wrap font-sans">
            {selfTestResult}
          </pre>
        ) : null}
        {status?.lastMetrics ? (
          <p className="text-xs text-gray-500">
            Last utterance: {formatMetrics(status.lastMetrics)}
          </p>
        ) : null}
        {testResult ? <p className="text-sm text-gray-700">{testResult}</p> : null}
        {saveState ? <p className="text-sm text-gray-500">{saveState}</p> : null}
      </section>
    </div>
  );
}

function PermissionRow({
  label,
  granted,
  onGrant,
  onOpen,
}: {
  label: string;
  granted?: boolean;
  onGrant: () => void;
  onOpen: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <div>
        <div className="text-sm font-medium">{label}</div>
        <div className={`text-xs ${granted ? 'text-gray-500' : 'text-rose-700'}`}>
          {granted === undefined ? 'Checking…' : granted ? 'Granted' : 'Required — not granted'}
        </div>
      </div>
      <div className="flex gap-2">
        <button
          type="button"
          className="text-xs rounded border border-gray-300 px-2 py-1 hover:bg-gray-50"
          onClick={onGrant}
        >
          Request
        </button>
        <button
          type="button"
          className="text-xs rounded border border-gray-300 px-2 py-1 hover:bg-gray-50"
          onClick={onOpen}
        >
          Open Settings
        </button>
      </div>
    </div>
  );
}
