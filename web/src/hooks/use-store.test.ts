import { describe, it, expect, beforeEach } from 'vitest'
import { useStore } from './use-store'

beforeEach(() => {
  window.location.hash = ''
  useStore.setState({
    selectedSessionId: null,
    selectedTimelineId: null,
    selectedStepId: null,
    sidebarCollapsed: false,
    view: 'sessions',
  })
})

describe('useStore', () => {
  it('has correct initial state', () => {
    const state = useStore.getState()
    expect(state.selectedSessionId).toBeNull()
    expect(state.selectedTimelineId).toBeNull()
    expect(state.selectedStepId).toBeNull()
    expect(state.sidebarCollapsed).toBe(false)
    expect(state.view).toBe('sessions')
  })

  it('selectSession sets session and clears step/timeline', () => {
    useStore.getState().selectStep('step-1')
    useStore.getState().selectTimeline('tl-1')
    useStore.getState().selectSession('session-abc')

    const state = useStore.getState()
    expect(state.selectedSessionId).toBe('session-abc')
    expect(state.selectedStepId).toBeNull()
    expect(state.selectedTimelineId).toBeNull()
    expect(state.view).toBe('sessions')
  })

  it('selectSession updates hash', () => {
    useStore.getState().selectSession('my-session-id')
    expect(window.location.hash).toBe('#/session/my-session-id')
  })

  it('selectSession(null) clears hash', () => {
    useStore.getState().selectSession('abc')
    useStore.getState().selectSession(null)
    expect(useStore.getState().selectedSessionId).toBeNull()
  })

  it('selectTimeline updates timeline', () => {
    useStore.getState().selectTimeline('tl-123')
    expect(useStore.getState().selectedTimelineId).toBe('tl-123')
  })

  it('selectStep sets and unsets step', () => {
    useStore.getState().selectStep('step-42')
    expect(useStore.getState().selectedStepId).toBe('step-42')

    useStore.getState().selectStep(null)
    expect(useStore.getState().selectedStepId).toBeNull()
  })

  it('toggleSidebar toggles collapsed state', () => {
    expect(useStore.getState().sidebarCollapsed).toBe(false)
    useStore.getState().toggleSidebar()
    expect(useStore.getState().sidebarCollapsed).toBe(true)
    useStore.getState().toggleSidebar()
    expect(useStore.getState().sidebarCollapsed).toBe(false)
  })

  it('setView changes active view', () => {
    useStore.getState().setView('baselines')
    expect(useStore.getState().view).toBe('baselines')
    useStore.getState().setView('diff')
    expect(useStore.getState().view).toBe('diff')
  })
})
