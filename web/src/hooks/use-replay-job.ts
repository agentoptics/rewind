import { useState, useCallback } from 'react'
import { useMutation } from '@tanstack/react-query'
import { api, type CreateReplayJobBody, type ReplayJobView } from '@/lib/api'
import { useWebSocket, type ReplayJobUpdateData } from '@/hooks/use-websocket'

/**
 * Hook backing the dashboard "Run replay" button (Phase 3 commit 8/13).
 *
 * Exposes:
 *   - `dispatch(body)` — POST /api/sessions/{id}/replay-jobs
 *   - `state` — derived from initial response + WebSocket updates
 *   - `progress` — { step, total }
 *   - `error` — last error_message
 *   - `cancel()` — operator cancel (DELETE /api/replay-jobs/{id})
 *
 * The button stays subscribed to the session's WebSocket channel
 * via the existing `useWebSocket` hook, filtering for
 * `replay_job_update` frames matching its job_id.
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

  const dispatchMut = useMutation({
    mutationFn: (body: CreateReplayJobBody) => {
      if (!sessionId) {
        return Promise.reject(new Error('no session selected'))
      }
      return api.createReplayJob(sessionId, body)
    },
    onSuccess: (resp) => {
      setJob({
        job_id: resp.job_id,
        state: resp.state,
        progress_step: 0,
        progress_total: null,
        error_message: null,
        error_stage: null,
        fork_timeline_id: resp.fork_timeline_id ?? null,
      })
    },
  })

  const cancelMut = useMutation({
    mutationFn: (jobId: string) => api.cancelReplayJob(jobId),
  })

  const onReplayJobUpdate = useCallback(
    (data: ReplayJobUpdateData) => {
      if (!job || data.job_id !== job.job_id) return
      setJob({
        job_id: job.job_id,
        state: data.state,
        progress_step: data.progress_step ?? job.progress_step,
        progress_total: data.progress_total ?? job.progress_total,
        error_message: data.error_message ?? null,
        error_stage: data.error_stage ?? null,
        fork_timeline_id: job.fork_timeline_id,
      })
    },
    [job],
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
    cancel: () => job && cancelMut.mutate(job.job_id),
    isCanceling: cancelMut.isPending,
    cancelError: cancelMut.error as Error | null,
    reset,
    connected,
  }
}

export type UseReplayJobReturn = ReturnType<typeof useReplayJob>

/** Convenience: fetch a single replay job (used for diagnostic panels). */
export async function fetchReplayJob(jobId: string): Promise<ReplayJobView> {
  return api.replayJob(jobId)
}
