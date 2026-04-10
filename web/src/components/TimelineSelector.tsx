import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { GitBranch, GitCommit } from 'lucide-react'
import type { Timeline } from '@/types/api'

interface TimelineSelectorProps {
  timelines: Timeline[]
}

export function TimelineSelector({ timelines }: TimelineSelectorProps) {
  const { selectedTimelineId, selectTimeline } = useStore()
  const root = timelines.find(t => !t.parent_timeline_id)
  const activeId = selectedTimelineId || root?.id

  return (
    <div className="flex items-center gap-2 px-4 py-2 border-b border-neutral-800 bg-neutral-950/50 overflow-x-auto scrollbar-thin">
      <GitBranch size={14} className="text-neutral-500 shrink-0" />
      {timelines.map((t) => {
        const isRoot = !t.parent_timeline_id
        const isActive = t.id === activeId

        return (
          <button
            key={t.id}
            onClick={() => selectTimeline(t.id)}
            className={cn(
              'flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs font-medium transition-colors shrink-0',
              isActive
                ? 'bg-neutral-800 text-neutral-100 border border-neutral-700'
                : 'text-neutral-500 hover:text-neutral-300 hover:bg-neutral-900 border border-transparent'
            )}
          >
            <GitCommit size={12} />
            <span>{t.label}</span>
            {t.fork_at_step && (
              <span className="text-[10px] text-neutral-600">@{t.fork_at_step}</span>
            )}
          </button>
        )
      })}
    </div>
  )
}
