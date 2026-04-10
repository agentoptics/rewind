import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn, formatTokens, formatDuration } from '@/lib/utils'
import { useState } from 'react'
import { ArrowLeft, Equal, Diff, ArrowLeftRight } from 'lucide-react'
import type { Timeline, TimelineDiff, StepDiffEntry } from '@/types/api'

export function DiffView({ sessionId }: { sessionId: string }) {
  const { setView } = useStore()
  const [leftId, setLeftId] = useState<string>('')
  const [rightId, setRightId] = useState<string>('')

  const { data: timelines = [] } = useQuery({
    queryKey: ['timelines', sessionId],
    queryFn: () => api.sessionTimelines(sessionId),
  })

  const canDiff = !!(leftId && rightId && leftId !== rightId)

  const { data: diff, isLoading: diffLoading } = useQuery({
    queryKey: ['diff', sessionId, leftId, rightId],
    queryFn: () => api.diffTimelines(sessionId, leftId, rightId),
    enabled: canDiff,
  })

  // Auto-select when timelines load
  if (timelines.length >= 2 && !leftId) {
    const root = timelines.find(t => !t.parent_timeline_id)
    const fork = timelines.find(t => t.parent_timeline_id)
    if (root) setLeftId(root.id)
    if (fork) setRightId(fork.id)
  }

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-neutral-800 px-4 py-3 flex items-center gap-3">
        <button onClick={() => setView('sessions')} className="text-neutral-400 hover:text-neutral-200">
          <ArrowLeft size={16} />
        </button>
        <h2 className="text-sm font-semibold text-neutral-200">Timeline Diff</h2>

        <div className="flex items-center gap-2 ml-auto">
          <TimelineSelect value={leftId} onChange={setLeftId} timelines={timelines} label="Left" />
          <ArrowLeftRight size={14} className="text-neutral-600" />
          <TimelineSelect value={rightId} onChange={setRightId} timelines={timelines} label="Right" />
        </div>
      </div>

      {diffLoading && <div className="flex-1 flex items-center justify-center text-neutral-500 text-sm">Computing diff...</div>}

      {diff && (
        <div className="flex-1 overflow-auto scrollbar-thin">
          {diff.diverge_at_step && (
            <div className="px-4 py-2 bg-amber-950/20 border-b border-amber-900/30 text-xs text-amber-400">
              Diverges at step {diff.diverge_at_step}
            </div>
          )}
          <div className="divide-y divide-neutral-800/50">
            {diff.step_diffs.map((entry) => (
              <DiffRow key={entry.step_number} entry={entry} diff={diff} />
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

function TimelineSelect({ value, onChange, timelines, label }: { value: string; onChange: (id: string) => void; timelines: Timeline[]; label: string }) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="bg-neutral-900 border border-neutral-700 rounded px-2 py-1 text-xs text-neutral-300 focus:outline-none focus:border-cyan-700"
    >
      <option value="">{label}</option>
      {timelines.map(t => (
        <option key={t.id} value={t.id}>{t.label} ({t.id.slice(0, 8)})</option>
      ))}
    </select>
  )
}

function DiffRow({ entry, diff }: { entry: StepDiffEntry; diff: TimelineDiff }) {
  const typeStyles: Record<string, string> = {
    Same: 'border-l-green-800',
    Modified: 'border-l-amber-500',
    LeftOnly: 'border-l-red-500',
    RightOnly: 'border-l-blue-500',
  }

  const typeLabel: Record<string, string> = {
    Same: 'Same',
    Modified: 'Modified',
    LeftOnly: `${diff.left_label} only`,
    RightOnly: `${diff.right_label} only`,
  }

  return (
    <div className={cn('flex items-start border-l-2 px-4 py-2.5', typeStyles[entry.diff_type])}>
      <div className="w-12 text-xs font-mono text-neutral-500 shrink-0">#{entry.step_number}</div>
      <div className="flex-1 grid grid-cols-2 gap-4">
        {entry.left ? (
          <StepSummaryCell summary={entry.left} />
        ) : (
          <div className="text-xs text-neutral-600 italic">-</div>
        )}
        {entry.right ? (
          <StepSummaryCell summary={entry.right} />
        ) : (
          <div className="text-xs text-neutral-600 italic">-</div>
        )}
      </div>
      <div className="w-20 text-right">
        <span className={cn('text-[10px] font-medium', {
          'text-green-500': entry.diff_type === 'Same',
          'text-amber-400': entry.diff_type === 'Modified',
          'text-red-400': entry.diff_type === 'LeftOnly',
          'text-blue-400': entry.diff_type === 'RightOnly',
        })}>
          {typeLabel[entry.diff_type]}
        </span>
      </div>
    </div>
  )
}

function StepSummaryCell({ summary }: { summary: { step_type: string; status: string; model: string; tokens_in: number; tokens_out: number; duration_ms: number; response_preview: string } }) {
  return (
    <div className="text-xs space-y-0.5">
      <div className="flex items-center gap-2">
        <span className="text-neutral-300 font-medium">{summary.step_type}</span>
        <span className="text-neutral-600 font-mono">{summary.model}</span>
      </div>
      <div className="text-neutral-500">
        {formatDuration(summary.duration_ms)} · {formatTokens(summary.tokens_in + summary.tokens_out)} tokens
      </div>
      {summary.response_preview && (
        <p className="text-neutral-600 truncate">{summary.response_preview}</p>
      )}
    </div>
  )
}
