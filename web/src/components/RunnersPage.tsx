import { useState } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { api, type RegisterRunnerResponse } from '@/lib/api'
import { Plus, Trash2, RefreshCw, Copy, Check, Wifi, WifiOff } from 'lucide-react'

/**
 * Runners management page (Phase 3 commit 7/13).
 *
 * Lists registered runners, lets operators register new ones,
 * regenerate tokens, and remove. Raw auth tokens are surfaced
 * **once** in a one-shot modal — operators get a copy-button and
 * a clear "save this now" warning.
 *
 * Uses TanStack Query for server-state caching. WebSocket
 * notifications aren't needed for this page (CRUD is operator-
 * driven, no real-time progress).
 */
export function RunnersPage() {
  const qc = useQueryClient()
  const [showRegister, setShowRegister] = useState(false)
  const [revealedToken, setRevealedToken] =
    useState<RegisterRunnerResponse | null>(null)

  const { data: runners = [], isLoading, error } = useQuery({
    queryKey: ['runners'],
    queryFn: api.runners,
    refetchInterval: 10_000,
  })

  const removeMut = useMutation({
    mutationFn: (id: string) => api.removeRunner(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['runners'] }),
  })

  const regenerateMut = useMutation({
    mutationFn: (id: string) => api.regenerateRunnerToken(id),
    onSuccess: (resp) => {
      setRevealedToken(resp)
      qc.invalidateQueries({ queryKey: ['runners'] })
    },
  })

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500">
        Loading runners...
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full overflow-hidden">
      <header className="px-6 py-4 border-b border-neutral-800 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold text-neutral-200">Runners</h1>
          <p className="text-xs text-neutral-500 mt-0.5">
            Registered agent processes that receive replay-job webhooks.
          </p>
        </div>
        <button
          onClick={() => setShowRegister(true)}
          className="flex items-center gap-2 px-3 py-1.5 bg-cyan-600 hover:bg-cyan-500 text-white text-sm rounded transition-colors"
        >
          <Plus size={14} /> Register runner
        </button>
      </header>

      {error ? (
        <div className="px-6 py-4 text-sm text-red-400">
          Failed to load runners: {(error as Error).message}
          {(error as Error).message.includes('503') && (
            <p className="mt-2 text-neutral-400 text-xs">
              Server bootstrap: <code className="text-cyan-300">REWIND_RUNNER_SECRET_KEY</code> env
              var must be set. Generate with{' '}
              <code className="text-cyan-300">openssl rand -base64 32</code>.
            </p>
          )}
        </div>
      ) : runners.length === 0 ? (
        <EmptyState onRegister={() => setShowRegister(true)} />
      ) : (
        <div className="flex-1 overflow-y-auto p-6 space-y-3">
          {runners.map((r) => (
            <div
              key={r.id}
              className="border border-neutral-800 rounded-lg p-4 bg-neutral-900/50"
            >
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-semibold text-neutral-200 truncate">
                      {r.name}
                    </h3>
                    <StatusPill status={r.status} />
                    <span className="text-xs text-neutral-500 font-mono">
                      {r.id.slice(0, 8)}
                    </span>
                  </div>
                  <div className="mt-1 text-xs text-neutral-400 space-y-0.5">
                    <div>
                      <span className="text-neutral-500">webhook:</span>{' '}
                      <code className="text-cyan-300">{r.webhook_url}</code>
                    </div>
                    <div>
                      <span className="text-neutral-500">token:</span>{' '}
                      <code className="text-neutral-400">
                        {r.auth_token_preview}
                      </code>
                      <span className="ml-3 text-neutral-500">
                        last seen:{' '}
                        {r.last_seen_at
                          ? new Date(r.last_seen_at).toLocaleString()
                          : 'never'}
                      </span>
                    </div>
                  </div>
                </div>
                <div className="flex gap-2 shrink-0">
                  <button
                    onClick={() => regenerateMut.mutate(r.id)}
                    disabled={regenerateMut.isPending}
                    className="p-1.5 rounded hover:bg-neutral-800 text-neutral-400 hover:text-cyan-300"
                    title="Regenerate token"
                  >
                    <RefreshCw size={14} />
                  </button>
                  <button
                    onClick={() => {
                      if (
                        confirm(
                          `Remove runner "${r.name}"? Active jobs (if any) will block deletion.`,
                        )
                      ) {
                        removeMut.mutate(r.id)
                      }
                    }}
                    disabled={removeMut.isPending}
                    className="p-1.5 rounded hover:bg-red-900/30 text-neutral-400 hover:text-red-400"
                    title="Remove runner"
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {removeMut.isError && (
        <ErrorToast
          msg={(removeMut.error as Error).message}
          onDismiss={() => removeMut.reset()}
        />
      )}
      {regenerateMut.isError && (
        <ErrorToast
          msg={(regenerateMut.error as Error).message}
          onDismiss={() => regenerateMut.reset()}
        />
      )}

      {showRegister && (
        <RegisterRunnerModal
          onClose={() => setShowRegister(false)}
          onSuccess={(resp) => {
            setRevealedToken(resp)
            setShowRegister(false)
            qc.invalidateQueries({ queryKey: ['runners'] })
          }}
        />
      )}

      {revealedToken && (
        <RawTokenModal
          token={revealedToken}
          onClose={() => setRevealedToken(null)}
        />
      )}
    </div>
  )
}

function StatusPill({ status }: { status: string }) {
  const cls =
    status === 'active'
      ? 'bg-green-900/40 text-green-300 border-green-700'
      : status === 'stale'
        ? 'bg-yellow-900/40 text-yellow-300 border-yellow-700'
        : 'bg-red-900/40 text-red-300 border-red-700'
  const Icon = status === 'active' ? Wifi : WifiOff
  return (
    <span
      className={`inline-flex items-center gap-1 px-2 py-0.5 text-[10px] font-semibold rounded border ${cls}`}
    >
      <Icon size={10} /> {status}
    </span>
  )
}

function EmptyState({ onRegister }: { onRegister: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center flex-1 text-neutral-500 gap-3">
      <p className="text-sm">No runners registered.</p>
      <button
        onClick={onRegister}
        className="flex items-center gap-2 px-3 py-1.5 bg-cyan-600 hover:bg-cyan-500 text-white text-sm rounded"
      >
        <Plus size={14} /> Register your first runner
      </button>
      <p className="text-xs text-neutral-600 mt-2">
        Or via CLI:{' '}
        <code className="text-cyan-300">
          rewind runners add --name X --webhook-url ...
        </code>
      </p>
    </div>
  )
}

function ErrorToast({
  msg,
  onDismiss,
}: {
  msg: string
  onDismiss: () => void
}) {
  return (
    <div className="fixed bottom-4 right-4 max-w-md p-3 bg-red-950 border border-red-800 rounded text-sm text-red-200 flex items-start gap-3 shadow-lg">
      <span className="flex-1">{msg}</span>
      <button onClick={onDismiss} className="text-red-400 hover:text-red-200">
        ✕
      </button>
    </div>
  )
}

function RegisterRunnerModal({
  onClose,
  onSuccess,
}: {
  onClose: () => void
  onSuccess: (resp: RegisterRunnerResponse) => void
}) {
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(null)
    setSubmitting(true)
    try {
      const resp = await api.registerRunner({
        name: name.trim(),
        mode: 'webhook',
        webhook_url: url.trim(),
      })
      onSuccess(resp)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50 p-4">
      <form
        onSubmit={submit}
        className="bg-neutral-900 border border-neutral-700 rounded-lg p-5 w-full max-w-md space-y-3"
      >
        <h2 className="text-base font-semibold text-neutral-200">
          Register runner
        </h2>
        <div>
          <label className="block text-xs text-neutral-400 mb-1">
            Name
            <span className="text-neutral-600"> (1-100 chars)</span>
          </label>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            required
            maxLength={100}
            className="w-full px-2 py-1.5 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200"
            placeholder="my-agent-runner"
          />
        </div>
        <div>
          <label className="block text-xs text-neutral-400 mb-1">
            Webhook URL
            <span className="text-neutral-600"> (http(s):// — public-routable)</span>
          </label>
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            required
            className="w-full px-2 py-1.5 bg-neutral-800 border border-neutral-700 rounded text-sm text-cyan-300"
            placeholder="https://your-agent.example.com/rewind-webhook"
          />
        </div>
        {error && (
          <div className="text-xs text-red-400 bg-red-950/40 border border-red-800 rounded p-2">
            {error}
          </div>
        )}
        <div className="flex justify-end gap-2 pt-2">
          <button
            type="button"
            onClick={onClose}
            className="px-3 py-1.5 text-sm text-neutral-400 hover:text-neutral-200"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={submitting || !name.trim() || !url.trim()}
            className="px-3 py-1.5 text-sm bg-cyan-600 hover:bg-cyan-500 disabled:bg-neutral-700 text-white rounded"
          >
            {submitting ? 'Registering...' : 'Register'}
          </button>
        </div>
      </form>
    </div>
  )
}

function RawTokenModal({
  token,
  onClose,
}: {
  token: RegisterRunnerResponse
  onClose: () => void
}) {
  const [copied, setCopied] = useState(false)

  const copy = () => {
    navigator.clipboard.writeText(token.raw_token).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    })
  }

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50 p-4">
      <div className="bg-neutral-900 border border-yellow-700 rounded-lg p-5 w-full max-w-2xl space-y-3">
        <h2 className="text-base font-semibold text-yellow-300">
          Save this raw token now
        </h2>
        <p className="text-xs text-neutral-400">
          {token.raw_token_warning}
        </p>
        <div className="flex items-stretch gap-2">
          <code className="flex-1 px-3 py-2 bg-black border border-neutral-700 rounded text-sm text-yellow-200 font-mono break-all">
            {token.raw_token}
          </code>
          <button
            onClick={copy}
            className="px-3 py-2 bg-cyan-600 hover:bg-cyan-500 text-white text-sm rounded flex items-center gap-2"
          >
            {copied ? <Check size={14} /> : <Copy size={14} />}
            {copied ? 'Copied' : 'Copy'}
          </button>
        </div>
        <div className="text-xs text-neutral-500 space-y-1 pt-2">
          <div>
            <span className="text-neutral-400">Runner id:</span>{' '}
            <code className="text-cyan-300">{token.runner.id}</code>
          </div>
          <div>
            <span className="text-neutral-400">Set in your runner:</span>{' '}
            <code className="text-cyan-300">
              export REWIND_RUNNER_TOKEN='&lt;paste here&gt;'
            </code>
          </div>
        </div>
        <div className="flex justify-end pt-2">
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-sm bg-neutral-700 hover:bg-neutral-600 text-white rounded"
          >
            I've saved it
          </button>
        </div>
      </div>
    </div>
  )
}
