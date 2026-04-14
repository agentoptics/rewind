import { useMemo, useRef, useCallback, useReducer, useEffect } from 'react'
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

// --- Viewport state (zoom/pan) ---

export interface ViewportState {
  zoom: number
  offset: number
  focusedLaneIndex: number | null
}

type ViewportAction =
  | { type: 'zoom_in' }
  | { type: 'zoom_out' }
  | { type: 'reset' }
  | { type: 'pan'; delta: number }
  | { type: 'set_offset'; offset: number }
  | { type: 'focus_lane'; index: number | null }
  | { type: 'wheel_zoom'; deltaY: number; cursorFraction: number; totalRange: number }

const ZOOM_FACTOR = 1.25
const MIN_ZOOM = 1
const MAX_ZOOM = 50

export function viewportReducer(state: ViewportState, action: ViewportAction): ViewportState {
  switch (action.type) {
    case 'zoom_in': {
      const zoom = Math.min(MAX_ZOOM, state.zoom * ZOOM_FACTOR)
      return { ...state, zoom }
    }
    case 'zoom_out': {
      const zoom = Math.max(MIN_ZOOM, state.zoom / ZOOM_FACTOR)
      return { ...state, zoom }
    }
    case 'reset':
      return { ...state, zoom: 1, offset: 0 }
    case 'pan':
      return { ...state, offset: state.offset + action.delta }
    case 'set_offset':
      return { ...state, offset: action.offset }
    case 'focus_lane':
      return { ...state, focusedLaneIndex: action.index }
    case 'wheel_zoom': {
      const direction = action.deltaY < 0 ? 1 : -1
      const newZoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, state.zoom * (direction > 0 ? ZOOM_FACTOR : 1 / ZOOM_FACTOR)))
      if (newZoom === state.zoom) return state
      const visibleRange = action.totalRange / state.zoom
      const cursorPos = state.offset + action.cursorFraction * visibleRange
      const newVisibleRange = action.totalRange / newZoom
      const newOffset = cursorPos - action.cursorFraction * newVisibleRange
      return { ...state, zoom: newZoom, offset: Math.max(0, newOffset) }
    }
    default:
      return state
  }
}

const INITIAL_VIEWPORT: ViewportState = { zoom: 1, offset: 0, focusedLaneIndex: null }

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
  const laneAreaRef = useRef<HTMLDivElement>(null)
  const isDragging = useRef(false)
  const dragStartX = useRef(0)
  const dragStartOffset = useRef(0)

  const [viewport, dispatch] = useReducer(viewportReducer, INITIAL_VIEWPORT)

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

  const totalRange = lanes[0]?.positionMode === 'step_number'
    ? sessionBounds.maxStep
    : sessionBounds.endMs - sessionBounds.startMs

  const handleBarClick = useCallback((stepId: string) => {
    onSelectStep(stepId === selectedStepId ? null : stepId)
  }, [onSelectStep, selectedStepId])

  // Wheel zoom handler
  const handleWheel = useCallback((e: React.WheelEvent) => {
    if (Math.abs(e.deltaY) < 4) return
    e.preventDefault()
    const rect = laneAreaRef.current?.getBoundingClientRect()
    if (!rect) return
    const cursorFraction = Math.max(0, Math.min(1, (e.clientX - rect.left - LABEL_WIDTH) / (rect.width - LABEL_WIDTH)))
    dispatch({ type: 'wheel_zoom', deltaY: e.deltaY, cursorFraction, totalRange })
  }, [totalRange])

  // Drag-to-pan handlers
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return
    isDragging.current = true
    dragStartX.current = e.clientX
    dragStartOffset.current = viewport.offset
    e.preventDefault()
  }, [viewport.offset])

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!isDragging.current || !laneAreaRef.current) return
      const rect = laneAreaRef.current.getBoundingClientRect()
      const barAreaWidth = rect.width - LABEL_WIDTH
      if (barAreaWidth <= 0) return
      const pxDelta = dragStartX.current - e.clientX
      const visibleRange = totalRange / viewport.zoom
      const rangeDelta = (pxDelta / barAreaWidth) * visibleRange
      dispatch({ type: 'set_offset', offset: Math.max(0, dragStartOffset.current + rangeDelta) })
    }
    const onUp = () => { isDragging.current = false }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
  }, [totalRange, viewport.zoom])

  // Keyboard navigation
  useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const handler = (e: KeyboardEvent) => {
      const allSteps = lanes.flatMap(l => l.steps)
      switch (e.key) {
        case '+': case '=': e.preventDefault(); dispatch({ type: 'zoom_in' }); break
        case '-': e.preventDefault(); dispatch({ type: 'zoom_out' }); break
        case '0': e.preventDefault(); dispatch({ type: 'reset' }); break
        case 'h': case 'ArrowLeft':
          if (!e.shiftKey) { e.preventDefault(); dispatch({ type: 'pan', delta: -totalRange / viewport.zoom * 0.15 }) }
          break
        case 'l': case 'ArrowRight':
          if (!e.shiftKey) { e.preventDefault(); dispatch({ type: 'pan', delta: totalRange / viewport.zoom * 0.15 }) }
          break
        case 'j': case 'ArrowDown': {
          e.preventDefault()
          if (e.shiftKey) {
            const next = viewport.focusedLaneIndex !== null
              ? Math.min(lanes.length - 1, viewport.focusedLaneIndex + 1)
              : 0
            dispatch({ type: 'focus_lane', index: next })
          } else {
            const currentIdx = allSteps.findIndex(s => s.id === selectedStepId)
            if (currentIdx < allSteps.length - 1) onSelectStep(allSteps[currentIdx + 1].id)
            else if (currentIdx === -1 && allSteps.length > 0) onSelectStep(allSteps[0].id)
          }
          break
        }
        case 'k': case 'ArrowUp': {
          e.preventDefault()
          if (e.shiftKey) {
            const prev = viewport.focusedLaneIndex !== null
              ? Math.max(0, viewport.focusedLaneIndex - 1)
              : 0
            dispatch({ type: 'focus_lane', index: prev })
          } else {
            const currentIdx = allSteps.findIndex(s => s.id === selectedStepId)
            if (currentIdx > 0) onSelectStep(allSteps[currentIdx - 1].id)
          }
          break
        }
        case 'Enter': break
        case 'Escape': e.preventDefault(); onSelectStep(null); break
      }
    }
    el.addEventListener('keydown', handler)
    return () => el.removeEventListener('keydown', handler)
  }, [lanes, selectedStepId, onSelectStep, viewport.zoom, viewport.focusedLaneIndex, totalRange])

  if (lanes.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm py-8">
        No activity to display
      </div>
    )
  }

  const visibleRange = totalRange / viewport.zoom
  const viewStart = viewport.offset
  const viewEnd = viewStart + visibleRange

  function toViewPct(value: number): number {
    return ((value - viewStart) / visibleRange) * 100
  }

  return (
    <div
      className="flex flex-col overflow-hidden h-full focus:outline-none"
      ref={containerRef}
      tabIndex={0}
    >
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-neutral-800 bg-neutral-900/50 shrink-0">
        <span className="text-[10px] uppercase tracking-wider font-semibold text-neutral-500">
          Activity Timeline
        </span>
        {isLive && (
          <span className="flex items-center gap-1 text-[10px] text-cyan-400">
            <span className="w-1.5 h-1.5 rounded-full bg-cyan-400 animate-pulse" />
            LIVE
          </span>
        )}
        {viewport.zoom > 1 && (
          <button
            onClick={() => dispatch({ type: 'reset' })}
            className="text-[10px] text-neutral-500 hover:text-neutral-300 transition-colors px-1.5 py-0.5 rounded border border-neutral-700/50 hover:border-neutral-600"
          >
            {viewport.zoom.toFixed(1)}x — Reset
          </button>
        )}
        <span className="ml-auto text-[10px] text-neutral-600">
          {lanes.length} {lanes.length === 1 ? 'lane' : 'lanes'} · {steps.length} steps
        </span>
      </div>

      {/* Minimap */}
      <Minimap
        lanes={lanes}
        sessionBounds={sessionBounds}
        totalRange={totalRange}
        viewport={viewport}
        onSetOffset={(offset) => dispatch({ type: 'set_offset', offset })}
      />

      {/* Swim lanes */}
      <div
        ref={laneAreaRef}
        className="flex-1 overflow-hidden select-none"
        style={{ cursor: isDragging.current ? 'grabbing' : 'grab' }}
        onWheel={handleWheel}
        onMouseDown={handleMouseDown}
      >
        <div className="relative" style={{ minHeight: lanes.length * LANE_HEIGHT + 28 }}>
          {lanes.map((lane, laneIdx) => {
            const bars = computeBarLayouts(lane.steps, lane.positionMode, sessionBounds)
            const isFocused = viewport.focusedLaneIndex === laneIdx

            return (
              <div
                key={lane.id}
                className={cn('flex', isFocused && 'bg-neutral-800/20')}
                style={{ height: LANE_HEIGHT }}
              >
                {/* Label column */}
                <div
                  className={cn(
                    'shrink-0 flex items-center gap-1.5 px-2 border-b border-r border-neutral-800/50',
                    'bg-neutral-900/80 sticky left-0 z-10',
                    isFocused && 'border-l-2 border-l-cyan-500',
                  )}
                  style={{ width: LABEL_WIDTH }}
                >
                  <span className={cn('w-2 h-2 rounded-full shrink-0', lane.color)} />
                  <span className={cn(
                    'text-[11px] truncate',
                    lane.isSubLane ? 'text-neutral-400' : 'text-neutral-200 font-medium',
                  )}>
                    {lane.isSubLane ? '↳ ' : ''}{lane.label}
                  </span>
                </div>

                {/* Bar area */}
                <div className="flex-1 relative border-b border-neutral-800/30 bg-neutral-950/30 overflow-hidden">
                  {bars.map(({ step, leftPct, widthPct }) => {
                    const barStart = leftPct
                    const barEnd = leftPct + widthPct
                    const viewStartPct = (viewStart / totalRange) * 100
                    const viewEndPct = (viewEnd / totalRange) * 100
                    if (barEnd < viewStartPct || barStart > viewEndPct) return null

                    const viewLeft = toViewPct(barStart * totalRange / 100)
                    const viewWidth = (widthPct / visibleRange * totalRange / 100) * 100
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
                          'absolute top-1 bottom-1 rounded-sm transition-colors',
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
                          left: `${viewLeft}%`,
                          width: `${viewWidth}%`,
                          minWidth: 4,
                        }}
                      >
                        {viewWidth > 3 && <StepTypeIcon type={step.step_type} />}
                        {viewWidth > 8 && (
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
              viewStart={viewStart}
              viewEnd={viewEnd}
              zoom={viewport.zoom}
            />
          </div>
        </div>
      </div>
    </div>
  )
}

// --- Minimap ---

function Minimap({
  lanes,
  sessionBounds,
  totalRange,
  viewport,
  onSetOffset,
}: {
  lanes: Lane[]
  sessionBounds: { startMs: number; endMs: number; maxStep: number }
  totalRange: number
  viewport: ViewportState
  onSetOffset: (offset: number) => void
}) {
  const minimapRef = useRef<HTMLDivElement>(null)
  const draggingMinimap = useRef(false)

  const viewportFraction = 1 / viewport.zoom
  const viewportLeftFraction = totalRange > 0 ? viewport.offset / totalRange : 0

  const handleMinimapMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return
    draggingMinimap.current = true
    const rect = minimapRef.current?.getBoundingClientRect()
    if (!rect) return
    const barArea = rect.width - LABEL_WIDTH
    const clickFraction = (e.clientX - rect.left - LABEL_WIDTH) / barArea
    const newOffset = Math.max(0, (clickFraction - viewportFraction / 2) * totalRange)
    onSetOffset(newOffset)
    e.preventDefault()
  }, [totalRange, viewportFraction, onSetOffset])

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!draggingMinimap.current || !minimapRef.current) return
      const rect = minimapRef.current.getBoundingClientRect()
      const barArea = rect.width - LABEL_WIDTH
      const clickFraction = (e.clientX - rect.left - LABEL_WIDTH) / barArea
      const newOffset = Math.max(0, (clickFraction - viewportFraction / 2) * totalRange)
      onSetOffset(newOffset)
    }
    const onUp = () => { draggingMinimap.current = false }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
  }, [totalRange, viewportFraction, onSetOffset])

  if (viewport.zoom <= 1) return null

  return (
    <div
      ref={minimapRef}
      className="border-b border-neutral-800/50 bg-neutral-950/50 cursor-pointer shrink-0 select-none"
      style={{ height: 36 }}
      onMouseDown={handleMinimapMouseDown}
    >
      <div className="flex h-full">
        <div className="shrink-0 flex items-center px-2 bg-neutral-900/80 border-r border-neutral-800/50" style={{ width: LABEL_WIDTH }}>
          <span className="text-[9px] text-neutral-600 uppercase tracking-wider">Overview</span>
        </div>
        <div className="flex-1 relative">
          {lanes.map((lane) => {
            const bars = computeBarLayouts(lane.steps, lane.positionMode, sessionBounds)
            const laneH = Math.max(2, (36 - 4) / lanes.length)
            const laneTop = (lanes.indexOf(lane)) * laneH + 2
            return bars.map(({ step, leftPct, widthPct }) => (
              <div
                key={step.id}
                className={cn('absolute rounded-[1px]', lane.color, 'opacity-40')}
                style={{
                  left: `${leftPct}%`,
                  width: `${widthPct}%`,
                  minWidth: 1,
                  top: laneTop,
                  height: Math.max(2, laneH - 1),
                }}
              />
            ))
          })}
          <div
            className="absolute top-0 bottom-0 border border-cyan-500/60 bg-cyan-500/10 rounded-sm pointer-events-none"
            style={{
              left: `${viewportLeftFraction * 100}%`,
              width: `${viewportFraction * 100}%`,
            }}
          />
        </div>
      </div>
    </div>
  )
}

// --- Time axis ---

function TimeAxis({
  positionMode,
  bounds,
  viewStart,
  viewEnd,
  zoom,
}: {
  positionMode: 'created_at' | 'step_number'
  bounds: { startMs: number; endMs: number; maxStep: number }
  viewStart: number
  viewEnd: number
  zoom: number
}) {
  const ticks = useMemo(() => {
    const count = Math.max(4, Math.min(12, Math.floor(6 * zoom)))
    const result: { pct: number; label: string }[] = []
    const visibleRange = viewEnd - viewStart
    if (visibleRange <= 0) return [{ pct: 0, label: '0s' }]

    if (positionMode === 'step_number') {
      const step = Math.max(1, Math.ceil(visibleRange / count))
      const start = Math.floor(viewStart / step) * step
      for (let i = start; i <= viewEnd; i += step) {
        const pct = ((i - viewStart) / visibleRange) * 100
        if (pct >= -5 && pct <= 105) {
          result.push({ pct, label: `#${Math.round(i) + 1}` })
        }
      }
    } else {
      const stepMs = visibleRange / count
      for (let i = 0; i <= count; i++) {
        const ms = viewStart + i * stepMs
        const pct = (i / count) * 100
        result.push({ pct, label: formatDuration(Math.round(ms)) })
      }
    }

    return result
  }, [positionMode, viewStart, viewEnd, zoom])

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
