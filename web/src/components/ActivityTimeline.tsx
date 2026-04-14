import { useMemo, useRef, useCallback } from 'react'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import { Brain, Wrench, ClipboardList, MessageSquare, Radio } from 'lucide-react'
import type { SpanResponse, StepResponse, Session } from '@/types/api'

// --- Lane building logic (exported for testing) ---

export interface Lane {
  id: string
  label: string
  isSubLane: boolean
  steps: StepResponse[]
  color: string
  positionMode: 'created_at' | 'step_number'
}

const LANE_COLORS = [
  'bg-cyan-500', 'bg-amber-500', 'bg-purple-500', 'bg-emerald-500',
  'bg-rose-500', 'bg-blue-500', 'bg-orange-500', 'bg-teal-500',
]

const TOOL_COLORS: Record<string, string> = {
  Read: 'bg-emerald-500',
  Write: 'bg-blue-500',
  Edit: 'bg-blue-400',
  Shell: 'bg-amber-500',
  Bash: 'bg-amber-500',
  Grep: 'bg-teal-500',
  Glob: 'bg-teal-400',
  Search: 'bg-violet-500',
  WebSearch: 'bg-violet-500',
  Agent: 'bg-cyan-500',
}

function getToolColor(toolName: string): string {
  return TOOL_COLORS[toolName] || LANE_COLORS[hashString(toolName) % LANE_COLORS.length]
}

function hashString(s: string): number {
  let hash = 0
  for (let i = 0; i < s.length; i++) {
    hash = ((hash << 5) - hash + s.charCodeAt(i)) | 0
  }
  return Math.abs(hash)
}

function collectSpanSteps(span: SpanResponse): StepResponse[] {
  const steps = [...span.steps]
  for (const child of span.child_spans) {
    if (child.span_type !== 'agent') {
      steps.push(...collectSpanSteps(child))
    }
  }
  return steps
}

function flattenAgentSpans(
  spans: SpanResponse[],
  parentIsAgent: boolean,
): { span: SpanResponse; isSubLane: boolean }[] {
  const result: { span: SpanResponse; isSubLane: boolean }[] = []
  for (const span of spans) {
    if (span.span_type === 'agent' || (!parentIsAgent && span.parent_span_id === null)) {
      result.push({ span, isSubLane: parentIsAgent })
      result.push(...flattenAgentSpans(span.child_spans, true))
    } else if (span.parent_span_id === null) {
      result.push({ span, isSubLane: false })
      result.push(...flattenAgentSpans(span.child_spans, true))
    }
  }
  return result
}

export function buildLanes(
  spans: SpanResponse[],
  steps: StepResponse[],
  session: Session,
): Lane[] {
  const isHookSession = session.source === 'hooks'

  if (spans.length > 0) {
    const agentEntries = flattenAgentSpans(spans, false)
    if (agentEntries.length === 0 && steps.length === 0) return []

    const lanes: Lane[] = agentEntries.map(({ span, isSubLane }, i) => ({
      id: span.id,
      label: span.name,
      isSubLane,
      steps: collectSpanSteps(span).sort((a, b) => a.step_number - b.step_number),
      color: LANE_COLORS[i % LANE_COLORS.length],
      positionMode: 'created_at' as const,
    }))

    const assignedStepIds = new Set(lanes.flatMap(l => l.steps.map(s => s.id)))
    const unassigned = steps.filter(s => !assignedStepIds.has(s.id))
    if (unassigned.length > 0) {
      if (lanes.length > 0) {
        lanes[0].steps.push(...unassigned)
        lanes[0].steps.sort((a, b) => a.step_number - b.step_number)
      } else {
        lanes.push({
          id: 'unassigned',
          label: 'Main',
          isSubLane: false,
          steps: unassigned.sort((a, b) => a.step_number - b.step_number),
          color: LANE_COLORS[0],
          positionMode: 'created_at',
        })
      }
    }

    return lanes
  }

  if (steps.length === 0) return []

  if (isHookSession) {
    const groups: Record<string, StepResponse[]> = {}
    const order: string[] = []

    for (const step of steps) {
      let key: string
      if (step.step_type === 'llm_call') key = 'LLM Calls'
      else if (step.step_type === 'user_prompt') key = 'Prompts'
      else if (step.tool_name) key = step.tool_name
      else key = step.step_type_label || 'Other'

      if (!groups[key]) {
        groups[key] = []
        order.push(key)
      }
      groups[key].push(step)
    }

    const priorityOrder = ['LLM Calls', 'Prompts']
    const sorted = [
      ...priorityOrder.filter(k => groups[k]),
      ...order.filter(k => !priorityOrder.includes(k)),
    ]

    return sorted.map((key, i) => ({
      id: `lane-${key}`,
      label: key,
      isSubLane: !['LLM Calls', 'Prompts'].includes(key),
      steps: groups[key],
      color: key === 'LLM Calls' ? 'bg-purple-500'
        : key === 'Prompts' ? 'bg-cyan-500'
        : getToolColor(key),
      positionMode: 'step_number' as const,
    }))
  }

  return [{
    id: 'main',
    label: 'Main',
    isSubLane: false,
    steps: steps.sort((a, b) => a.step_number - b.step_number),
    color: LANE_COLORS[0],
    positionMode: 'created_at',
  }]
}

// --- Bar positioning ---

interface BarLayout {
  step: StepResponse
  leftPct: number
  widthPct: number
}

function computeBarLayouts(
  steps: StepResponse[],
  positionMode: 'created_at' | 'step_number',
  sessionBounds: { startMs: number; endMs: number; maxStep: number },
): BarLayout[] {
  if (steps.length === 0) return []

  if (positionMode === 'step_number') {
    const { maxStep } = sessionBounds
    if (maxStep <= 0) return []
    return steps.map(step => ({
      step,
      leftPct: ((step.step_number - 1) / maxStep) * 100,
      widthPct: Math.max(0.4, (1 / maxStep) * 100 * Math.min(1, step.duration_ms / 1000 + 0.3)),
    }))
  }

  const { startMs, endMs } = sessionBounds
  const totalMs = endMs - startMs
  if (totalMs <= 0) return steps.map(step => ({ step, leftPct: 0, widthPct: 100 }))

  return steps.map(step => {
    const stepStart = new Date(step.created_at).getTime()
    const leftPct = ((stepStart - startMs) / totalMs) * 100
    const widthPct = Math.max(0.3, (step.duration_ms / totalMs) * 100)
    return { step, leftPct, widthPct }
  })
}

// --- Step type icon ---

function StepTypeIcon({ type }: { type: string }) {
  switch (type) {
    case 'llm_call': return <Brain size={10} className="text-purple-300" />
    case 'tool_call': return <Wrench size={10} className="text-amber-300" />
    case 'tool_result': return <ClipboardList size={10} className="text-blue-300" />
    case 'user_prompt': return <MessageSquare size={10} className="text-cyan-300" />
    default: return <Radio size={10} className="text-neutral-400" />
  }
}

// --- Component ---

interface ActivityTimelineProps {
  spans: SpanResponse[]
  steps: StepResponse[]
  session: Session
  selectedStepId: string | null
  onSelectStep: (id: string | null) => void
  isLive?: boolean
}

const LANE_HEIGHT = 36
const LABEL_WIDTH = 160

export function ActivityTimeline({
  spans,
  steps,
  session,
  selectedStepId,
  onSelectStep,
  isLive,
}: ActivityTimelineProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  const lanes = useMemo(() => buildLanes(spans, steps, session), [spans, steps, session])

  const sessionBounds = useMemo(() => {
    const allSteps = lanes.flatMap(l => l.steps)
    if (allSteps.length === 0) return { startMs: 0, endMs: 0, maxStep: 0 }

    const times = allSteps.map(s => new Date(s.created_at).getTime())
    const endTimes = allSteps.map(s => new Date(s.created_at).getTime() + s.duration_ms)
    return {
      startMs: Math.min(...times),
      endMs: Math.max(...endTimes),
      maxStep: Math.max(...allSteps.map(s => s.step_number)),
    }
  }, [lanes])

  const handleBarClick = useCallback((stepId: string) => {
    onSelectStep(stepId === selectedStepId ? null : stepId)
  }, [onSelectStep, selectedStepId])

  if (lanes.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm py-8">
        No activity to display
      </div>
    )
  }

  return (
    <div className="flex flex-col overflow-hidden" ref={containerRef}>
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-neutral-800 bg-neutral-900/50">
        <span className="text-[10px] uppercase tracking-wider font-semibold text-neutral-500">
          Activity Timeline
        </span>
        {isLive && (
          <span className="flex items-center gap-1 text-[10px] text-cyan-400">
            <span className="w-1.5 h-1.5 rounded-full bg-cyan-400 animate-pulse" />
            LIVE
          </span>
        )}
        <span className="ml-auto text-[10px] text-neutral-600">
          {lanes.length} {lanes.length === 1 ? 'lane' : 'lanes'} · {steps.length} steps
        </span>
      </div>

      {/* Swim lanes */}
      <div className="flex-1 overflow-auto">
        <div className="relative" style={{ minHeight: lanes.length * LANE_HEIGHT + 32 }}>
          {lanes.map((lane, laneIdx) => {
            const bars = computeBarLayouts(lane.steps, lane.positionMode, sessionBounds)

            return (
              <div
                key={lane.id}
                className="flex"
                style={{ height: LANE_HEIGHT }}
              >
                {/* Label column */}
                <div
                  className={cn(
                    'shrink-0 flex items-center gap-1.5 px-2 border-b border-r border-neutral-800/50',
                    'bg-neutral-900/80 sticky left-0 z-10',
                  )}
                  style={{ width: LABEL_WIDTH }}
                >
                  <span
                    className={cn('w-2 h-2 rounded-full shrink-0', lane.color)}
                  />
                  <span className={cn(
                    'text-[11px] truncate',
                    lane.isSubLane ? 'text-neutral-400' : 'text-neutral-200 font-medium',
                  )}>
                    {lane.isSubLane ? '↳ ' : ''}{lane.label}
                  </span>
                </div>

                {/* Bar area */}
                <div className="flex-1 relative border-b border-neutral-800/30 bg-neutral-950/30">
                  {bars.map(({ step, leftPct, widthPct }) => {
                    const isSelected = step.id === selectedStepId
                    const isError = step.status === 'error'

                    return (
                      <button
                        key={step.id}
                        onClick={() => handleBarClick(step.id)}
                        title={[
                          step.tool_name || step.step_type_label,
                          step.model,
                          formatDuration(step.duration_ms),
                          step.tokens_in + step.tokens_out > 0
                            ? `${formatTokens(step.tokens_in + step.tokens_out)} tok`
                            : null,
                          step.error ? `Error: ${step.error}` : null,
                        ].filter(Boolean).join(' · ')}
                        className={cn(
                          'absolute top-1 bottom-1 rounded-sm transition-all',
                          'flex items-center gap-0.5 overflow-hidden px-0.5',
                          isSelected
                            ? 'ring-2 ring-cyan-400 brightness-125 z-20'
                            : 'hover:brightness-110 z-10',
                          isError ? 'ring-1 ring-red-500/60' : '',
                          lane.color,
                          isError ? 'opacity-80' : 'opacity-70',
                          isSelected && 'opacity-100',
                        )}
                        style={{
                          left: `${leftPct}%`,
                          width: `${widthPct}%`,
                          minWidth: 4,
                        }}
                      >
                        {widthPct > 3 && <StepTypeIcon type={step.step_type} />}
                        {widthPct > 8 && (
                          <span className="text-[9px] text-white/80 truncate">
                            {step.tool_name || step.step_type_label}
                          </span>
                        )}
                      </button>
                    )
                  })}
                </div>
              </div>
            )
          })}

          {/* Time axis */}
          <div
            className="flex items-center border-t border-neutral-800/50"
            style={{ paddingLeft: LABEL_WIDTH, height: 24 }}
          >
            <TimeAxis
              positionMode={lanes[0]?.positionMode ?? 'created_at'}
              bounds={sessionBounds}
            />
          </div>
        </div>
      </div>
    </div>
  )
}

function TimeAxis({
  positionMode,
  bounds,
}: {
  positionMode: 'created_at' | 'step_number'
  bounds: { startMs: number; endMs: number; maxStep: number }
}) {
  const ticks = useMemo(() => {
    const count = 6
    const result: { pct: number; label: string }[] = []

    if (positionMode === 'step_number') {
      const step = Math.max(1, Math.ceil(bounds.maxStep / count))
      for (let i = 0; i <= bounds.maxStep; i += step) {
        result.push({ pct: (i / bounds.maxStep) * 100, label: `#${i + 1}` })
      }
    } else {
      const totalMs = bounds.endMs - bounds.startMs
      if (totalMs <= 0) return [{ pct: 0, label: '0s' }]
      const stepMs = totalMs / count
      for (let i = 0; i <= count; i++) {
        const ms = i * stepMs
        result.push({ pct: (ms / totalMs) * 100, label: formatDuration(Math.round(ms)) })
      }
    }

    return result
  }, [positionMode, bounds])

  return (
    <div className="relative w-full h-full">
      {ticks.map(({ pct, label }, i) => (
        <span
          key={i}
          className="absolute text-[9px] text-neutral-600 -translate-x-1/2"
          style={{ left: `${pct}%`, top: 4 }}
        >
          {label}
        </span>
      ))}
    </div>
  )
}
