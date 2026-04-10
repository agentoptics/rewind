import type {
  Session, SessionDetail, StepResponse, StepDetail,
  Baseline, BaselineDetail, CacheStats, Snapshot,
  Timeline, TimelineDiff,
} from '@/types/api'

const BASE = '/api'

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`)
  if (!res.ok) {
    const text = await res.text()
    throw new Error(`API error ${res.status}: ${text}`)
  }
  return res.json()
}

export const api = {
  health: () => get<{ status: string; version: string }>('/health'),
  sessions: () => get<Session[]>('/sessions'),
  session: (id: string) => get<SessionDetail>(`/sessions/${id}`),
  sessionSteps: (id: string, timeline?: string) => {
    const q = timeline ? `?timeline=${encodeURIComponent(timeline)}` : ''
    return get<StepResponse[]>(`/sessions/${id}/steps${q}`)
  },
  sessionTimelines: (id: string) => get<Timeline[]>(`/sessions/${id}/timelines`),
  stepDetail: (id: string) => get<StepDetail>(`/steps/${id}`),
  diffTimelines: (sessionId: string, left: string, right: string) =>
    get<TimelineDiff>(`/sessions/${sessionId}/diff?left=${encodeURIComponent(left)}&right=${encodeURIComponent(right)}`),
  baselines: () => get<Baseline[]>('/baselines'),
  baseline: (name: string) => get<BaselineDetail>(`/baselines/${name}`),
  cacheStats: () => get<CacheStats>('/cache/stats'),
  snapshots: () => get<Snapshot[]>('/snapshots'),
}
