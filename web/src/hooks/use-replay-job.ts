import { useState, useCallback } from 'react'
import { useMutation } from '@tanstack/react-query'
import { api, type CreateReplayJobBody, type ReplayJobView } from '@/lib/api'
import { useWebSocket, type ReplayJobUpdateData } from '@/hooks/use-websocket'

/**
 * Hook backing the dashboard "Run replay" button (Phase 3 commit 8/13).
 *
 * **Review #154 F7:** WebSocket events are treated as INVALIDATION
 * (refetch state) rather than the only source of truth. After
 * dispatch returns 202, the hook immediately fetches the current
 * job state via GET /api/replay-jobs/{id} so it doesn't miss
 * fast jobs that complete before the WS subscription is fully
 * wired. Each subsequent WS frame triggers another fetch
 * (cheap; the job row is small) — that way out-of-order frames
 * + missing frames both resolve to "the latest state on the
 * server".
 *
 * **Review #154 N1:** cancellation removed (deferred to v3.1).
 */
export function useReplayJob(sessionId: string | null) {
  const [job, setJob] = useState<{
    job_id: string
    state: string
    progress_step: number
    progress_total: number | null
    error_message: string | null
    error_stage: string | null
    fork_timeline_id: string | null
  } | null>(null)

  const refreshJob = useCallback(async (jobId: string) => {
    try {
      const r = await api.replayJob(jobId)
      setJob((prev) => ({
        job_id: r.id,
        state: r.state,
        progress_step: r.progress_step,
        progress_total: r.progress_total,
        error_message: r.error_message,
        error_stage: r.error_stage,
        fork_timeline_id: prev?.fork_timeline_id ?? null,
      }))
    } catch {
      // Polling failure is non-fatal — WS will eventually re-trigger.
    }
  }, [])

  const dispatchMut = useMutation({
    mutationFn: (body: CreateReplayJobBody) => {
      if (!sessionId) {
        return Promise.reject(new Error('no session selected'))
      }
      return api.createReplayJob(sessionId, body)
    },
    onSuccess: async (resp) => {
      setJob({
        job_id: resp.job_id,
        state: resp.state,
        progress_step: 0,
        progress_total: null,
        error_message: null,
        error_stage: null,
        fork_timeline_id: resp.fork_timeline_id ?? null,
      })
      // F7: fast-job race fix — fetch current state immediately
      // in case the dispatcher already advanced past `pending`
      // before the WebSocket subscription is wired up.
      void refreshJob(resp.job_id)
    },
  })

  const onReplayJobUpdate = useCallback(
    (data: ReplayJobUpdateData) => {
      if (!job || data.job_id !== job.job_id) return
      // Treat the WS frame as invalidation; refetch authoritative
      // state. Avoids reasoning about out-of-order frames or
      // partial-update bugs on the WS path.
      void refreshJob(job.job_id)
    },
    [job, refreshJob],
  )

  const { connected } = useWebSocket({ sessionId, onReplayJobUpdate })

  const reset = useCallback(() => {
    setJob(null)
    dispatchMut.reset()
  }, [dispatchMut])

  return {
    dispatch: dispatchMut.mutate,
    isDispatching: dispatchMut.isPending,
    dispatchError: dispatchMut.error as Error | null,
    job,
    reset,
    connected,
  }
}

export type UseReplayJobReturn = ReturnType<typeof useReplayJob>

/** Convenience: fetch a single replay job (used for diagnostic panels). */
export async function fetchReplayJob(jobId: string): Promise<ReplayJobView> {
  return api.replayJob(jobId)
}
