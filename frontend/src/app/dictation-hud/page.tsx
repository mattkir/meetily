'use client';

import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';

type HudPayload = {
  phase: string;
  message: string;
  partial?: string | null;
  metrics?: {
    graceMs: number;
    resampleMs: number;
    ipcMs: number;
    asrMs: number;
    polishMs: number;
    insertMs: number;
    e2eMs: number;
  } | null;
};

const PHASE_STYLES: Record<string, string> = {
  listening: 'bg-emerald-600/95',
  processing: 'bg-sky-600/95',
  error: 'bg-rose-600/95',
  blocked: 'bg-amber-600/95',
  idle: 'bg-transparent',
};

export default function DictationHudPage() {
  const [payload, setPayload] = useState<HudPayload>({
    phase: 'idle',
    message: '',
  });

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<HudPayload>('dictation-hud', (event) => {
      setPayload(event.payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  if (payload.phase === 'idle' || !payload.message) {
    return <main className="h-screen w-screen bg-transparent" />;
  }

  const bg = PHASE_STYLES[payload.phase] ?? 'bg-slate-800/95';

  return (
    <main className="h-screen w-screen bg-transparent flex items-center justify-center select-none">
      <div
        className={`${bg} text-white px-5 py-3 rounded-xl shadow-lg text-sm font-medium tracking-wide flex flex-col gap-1 min-w-[240px]`}
      >
        <span className="uppercase text-[10px] opacity-80 tracking-widest">
          {payload.phase}
        </span>
        <span>{payload.message}</span>
        {payload.partial ? (
          <span className="text-xs opacity-80 truncate max-w-[280px]">
            {payload.partial}
          </span>
        ) : null}
      </div>
    </main>
  );
}
