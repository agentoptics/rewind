import { useEffect, useRef, useCallback, useState } from 'react'
import type { WsServerMessage, StepResponse } from '@/types/api'
import { getToken } from '@/lib/auth'

export interface ReplayJobUpdateData {
  job_id: string
  session_id: string
  state: string
  progress_step?: number
  progress_total?: number
  error_message?: string
  error_stage?: string
}

interface UseWebSocketOptions {
  sessionId: string | null
  onStep?: (step: StepResponse) => void
  onSessionUpdate?: (data: { session_id: string; status: string; total_steps: number; total_tokens: number }) => void
  /** Phase 3 commit 8: replay-job state/progress updates. */
  onReplayJobUpdate?: (data: ReplayJobUpdateData) => void
}

export function useWebSocket({ sessionId, onStep, onSessionUpdate, onReplayJobUpdate }: UseWebSocketOptions) {
  const wsRef = useRef<WebSocket | null>(null)
  const [connected, setConnected] = useState(false)
  const reconnectTimeout = useRef<ReturnType<typeof setTimeout>>(undefined)

  const connect = useCallback(() => {
    if (wsRef.current?.readyState === WebSocket.OPEN) return

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    // Browsers can't set Authorization on WebSocket upgrades — the server
    // accepts ?token= as a fallback scoped to /api/ws only. See
    // crates/rewind-web/src/auth.rs::extract_token.
    const token = getToken()
    const qs = token ? `?token=${encodeURIComponent(token)}` : ''
    const ws = new WebSocket(`${protocol}//${window.location.host}/api/ws${qs}`)

    ws.onopen = () => {
      setConnected(true)
      if (sessionId) {
        ws.send(JSON.stringify({ type: 'subscribe', session_id: sessionId }))
      }
    }

    ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data) as
          | WsServerMessage
          | { type: 'replay_job_update'; data: ReplayJobUpdateData }
        switch (msg.type) {
          case 'step':
            onStep?.((msg as { data: StepResponse }).data)
            break
          case 'session_update':
            onSessionUpdate?.((msg as { data: { session_id: string; status: string; total_steps: number; total_tokens: number } }).data)
            break
          case 'replay_job_update':
            onReplayJobUpdate?.(msg.data)
            break
        }
      } catch {
        // ignore malformed messages
      }
    }

    ws.onclose = () => {
      setConnected(false)
      reconnectTimeout.current = setTimeout(connect, 3000)
    }

    ws.onerror = () => {
      ws.close()
    }

    wsRef.current = ws
  }, [sessionId, onStep, onSessionUpdate, onReplayJobUpdate])

  useEffect(() => {
    connect()
    return () => {
      clearTimeout(reconnectTimeout.current)
      wsRef.current?.close()
    }
  }, [connect])

  useEffect(() => {
    const ws = wsRef.current
    if (ws?.readyState === WebSocket.OPEN && sessionId) {
      ws.send(JSON.stringify({ type: 'subscribe', session_id: sessionId }))
    }
  }, [sessionId])

  return { connected }
}
