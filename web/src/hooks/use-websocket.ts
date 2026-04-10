import { useEffect, useRef, useCallback, useState } from 'react'
import type { WsServerMessage, StepResponse } from '@/types/api'

interface UseWebSocketOptions {
  sessionId: string | null
  onStep?: (step: StepResponse) => void
  onSessionUpdate?: (data: { session_id: string; status: string; total_steps: number; total_tokens: number }) => void
}

export function useWebSocket({ sessionId, onStep, onSessionUpdate }: UseWebSocketOptions) {
  const wsRef = useRef<WebSocket | null>(null)
  const [connected, setConnected] = useState(false)
  const reconnectTimeout = useRef<ReturnType<typeof setTimeout>>(undefined)

  const connect = useCallback(() => {
    if (wsRef.current?.readyState === WebSocket.OPEN) return

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const ws = new WebSocket(`${protocol}//${window.location.host}/api/ws`)

    ws.onopen = () => {
      setConnected(true)
      if (sessionId) {
        ws.send(JSON.stringify({ type: 'subscribe', session_id: sessionId }))
      }
    }

    ws.onmessage = (event) => {
      try {
        const msg: WsServerMessage = JSON.parse(event.data)
        switch (msg.type) {
          case 'step':
            onStep?.(msg.data)
            break
          case 'session_update':
            onSessionUpdate?.(msg.data)
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
  }, [sessionId, onStep, onSessionUpdate])

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
