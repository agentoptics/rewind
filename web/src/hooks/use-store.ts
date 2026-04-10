import { create } from 'zustand'

interface UIState {
  selectedSessionId: string | null
  selectedTimelineId: string | null
  selectedStepId: string | null
  sidebarCollapsed: boolean
  view: 'sessions' | 'diff' | 'baselines'

  selectSession: (id: string | null) => void
  selectTimeline: (id: string | null) => void
  selectStep: (id: string | null) => void
  toggleSidebar: () => void
  setView: (view: UIState['view']) => void
}

function parseHash(): { sessionId: string | null; stepId: string | null } {
  const hash = window.location.hash.slice(1)
  const parts = hash.split('/')
  return {
    sessionId: parts[1] || null,
    stepId: parts[2] || null,
  }
}

export const useStore = create<UIState>((set) => {
  const initial = parseHash()
  return {
    selectedSessionId: initial.sessionId,
    selectedTimelineId: null,
    selectedStepId: initial.stepId,
    sidebarCollapsed: false,
    view: 'sessions',

    selectSession: (id) => {
      set({ selectedSessionId: id, selectedStepId: null, selectedTimelineId: null, view: 'sessions' })
      window.location.hash = id ? `#/session/${id}` : ''
    },
    selectTimeline: (id) => set({ selectedTimelineId: id }),
    selectStep: (id) => set({ selectedStepId: id }),
    toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
    setView: (view) => set({ view }),
  }
})
