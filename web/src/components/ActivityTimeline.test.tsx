import { describe, it, expect } from 'vitest'
import { buildLanes, viewportReducer, type Lane, type ViewportState } from './ActivityTimeline'
import type { SpanResponse, StepResponse, Session } from '@/types/api'

function makeStep(overrides: Partial<StepResponse> = {}): StepResponse {
  return {
    id: 'step-1',
    timeline_id: 'tl-1',
    session_id: 's-1',
    step_number: 1,
    step_type: 'tool_call',
    step_type_label: 'Tool Call',
    step_type_icon: '🔧',
    status: 'success',
    created_at: '2026-04-14T10:00:00Z',
    duration_ms: 100,
    tokens_in: 0,
    tokens_out: 0,
    model: '',
    error: null,
    response_preview: '',
    ...overrides,
  }
}

function makeSpan(overrides: Partial<SpanResponse> = {}): SpanResponse {
  return {
    id: 'span-1',
    session_id: 's-1',
    timeline_id: 'tl-1',
    parent_span_id: null,
    span_type: 'agent',
    span_type_icon: '🤖',
    name: 'orchestrator',
    status: 'completed',
    started_at: '2026-04-14T10:00:00Z',
    ended_at: '2026-04-14T10:00:05Z',
    duration_ms: 5000,
    metadata: {},
    error: null,
    child_spans: [],
    steps: [],
    ...overrides,
  }
}

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: 's-1',
    name: 'test',
    created_at: '2026-04-14T10:00:00Z',
    updated_at: '2026-04-14T10:00:00Z',
    status: 'Completed',
    total_steps: 0,
    total_tokens: 0,
    metadata: {},
    source: 'proxy',
    ...overrides,
  }
}

describe('buildLanes', () => {
  describe('Mode A: sessions with agent spans', () => {
    it('creates one lane per agent span', () => {
      const spans: SpanResponse[] = [
        makeSpan({
          id: 'agent-1',
          name: 'supervisor',
          steps: [makeStep({ id: 's1', step_number: 1 })],
          child_spans: [
            makeSpan({
              id: 'agent-2',
              name: 'researcher',
              span_type: 'agent',
              parent_span_id: 'agent-1',
              steps: [makeStep({ id: 's2', step_number: 2 }), makeStep({ id: 's3', step_number: 3 })],
            }),
          ],
        }),
      ]

      const lanes = buildLanes(spans, [], makeSession())
      expect(lanes).toHaveLength(2)
      expect(lanes[0].label).toBe('supervisor')
      expect(lanes[0].isSubLane).toBe(false)
      expect(lanes[0].steps).toHaveLength(1)
      expect(lanes[1].label).toBe('researcher')
      expect(lanes[1].isSubLane).toBe(true)
      expect(lanes[1].steps).toHaveLength(2)
    })

    it('handles root-level non-agent spans from OTel imports', () => {
      const spans: SpanResponse[] = [
        makeSpan({
          id: 'otel-root',
          name: 'http-server',
          span_type: 'http',
          steps: [makeStep({ id: 's1' })],
        }),
      ]

      const lanes = buildLanes(spans, [], makeSession())
      expect(lanes).toHaveLength(1)
      expect(lanes[0].label).toBe('http-server')
    })
  })

  describe('Mode B: hook sessions without spans', () => {
    it('groups steps by tool_name into sub-lanes', () => {
      const steps: StepResponse[] = [
        makeStep({ id: 's1', step_number: 1, step_type: 'tool_call', tool_name: 'Read' }),
        makeStep({ id: 's2', step_number: 2, step_type: 'tool_call', tool_name: 'Write' }),
        makeStep({ id: 's3', step_number: 3, step_type: 'tool_call', tool_name: 'Read' }),
        makeStep({ id: 's4', step_number: 4, step_type: 'llm_call', tool_name: undefined }),
        makeStep({ id: 's5', step_number: 5, step_type: 'user_prompt', tool_name: undefined }),
      ]

      const lanes = buildLanes([], steps, makeSession({ source: 'hooks' }))
      const labels = lanes.map(l => l.label)
      expect(labels).toContain('LLM Calls')
      expect(labels).toContain('Read')
      expect(labels).toContain('Write')
      expect(labels).toContain('Prompts')

      const readLane = lanes.find(l => l.label === 'Read')!
      expect(readLane.steps).toHaveLength(2)
    })

    it('uses step_number positioning for hook sessions', () => {
      const steps: StepResponse[] = [
        makeStep({ id: 's1', step_number: 1, step_type: 'tool_call', tool_name: 'Read' }),
        makeStep({ id: 's2', step_number: 2, step_type: 'tool_call', tool_name: 'Read' }),
      ]

      const lanes = buildLanes([], steps, makeSession({ source: 'hooks' }))
      expect(lanes[0].positionMode).toBe('step_number')
    })

    it('uses created_at positioning for proxy sessions', () => {
      const spans: SpanResponse[] = [
        makeSpan({ steps: [makeStep()] }),
      ]

      const lanes = buildLanes(spans, [], makeSession({ source: 'proxy' }))
      expect(lanes[0].positionMode).toBe('created_at')
    })
  })

  describe('edge cases', () => {
    it('returns empty array for no steps and no spans', () => {
      const lanes = buildLanes([], [], makeSession())
      expect(lanes).toHaveLength(0)
    })

    it('collects steps without a span into a fallback lane', () => {
      const spans: SpanResponse[] = [
        makeSpan({
          id: 'agent-1',
          name: 'main',
          steps: [],
          child_spans: [],
        }),
      ]
      const steps: StepResponse[] = [
        makeStep({ id: 's1', step_number: 1 }),
      ]

      const lanes = buildLanes(spans, steps, makeSession())
      const hasSteps = lanes.some(l => l.steps.length > 0)
      expect(hasSteps).toBe(true)
    })
  })
})

describe('viewportReducer', () => {
  const initial: ViewportState = { zoom: 1, offset: 0, focusedLaneIndex: null }

  it('zooms in, clamped to max 50', () => {
    const state = viewportReducer(initial, { type: 'zoom_in' })
    expect(state.zoom).toBeGreaterThan(1)

    let s = { ...initial, zoom: 50 }
    s = viewportReducer(s, { type: 'zoom_in' })
    expect(s.zoom).toBe(50)
  })

  it('zooms out, clamped to min 1', () => {
    const state = viewportReducer({ ...initial, zoom: 5 }, { type: 'zoom_out' })
    expect(state.zoom).toBeLessThan(5)

    let s = { ...initial, zoom: 1 }
    s = viewportReducer(s, { type: 'zoom_out' })
    expect(s.zoom).toBe(1)
  })

  it('resets zoom to 1', () => {
    const state = viewportReducer({ ...initial, zoom: 10, offset: 500 }, { type: 'reset' })
    expect(state.zoom).toBe(1)
    expect(state.offset).toBe(0)
  })

  it('pans by delta', () => {
    const state = viewportReducer(initial, { type: 'pan', delta: 100 })
    expect(state.offset).toBe(100)
  })

  it('sets offset directly', () => {
    const state = viewportReducer(initial, { type: 'set_offset', offset: 250 })
    expect(state.offset).toBe(250)
  })

  it('focuses a lane by index', () => {
    const state = viewportReducer(initial, { type: 'focus_lane', index: 2 })
    expect(state.focusedLaneIndex).toBe(2)
  })

  it('handles wheel zoom at a cursor position', () => {
    const state = viewportReducer(
      { zoom: 2, offset: 100, focusedLaneIndex: null },
      { type: 'wheel_zoom', deltaY: -100, cursorFraction: 0.5, totalRange: 10000 }
    )
    expect(state.zoom).toBeGreaterThan(2)
  })
})
