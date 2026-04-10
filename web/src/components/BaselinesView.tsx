import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { cn, timeAgo, formatTokens } from '@/lib/utils'
import { useState } from 'react'
import { Shield, ChevronRight, CheckCircle2, XCircle, AlertTriangle } from 'lucide-react'
import type { Baseline, BaselineDetail, BaselineStep } from '@/types/api'

export function BaselinesView() {
  const [selectedName, setSelectedName] = useState<string | null>(null)

  const { data: baselines = [], isLoading } = useQuery({
    queryKey: ['baselines'],
    queryFn: api.baselines,
  })

  const { data: detail } = useQuery({
    queryKey: ['baseline', selectedName],
    queryFn: () => api.baseline(selectedName!),
    enabled: !!selectedName,
  })

  return (
    <div className="flex h-full">
      <div className="w-80 border-r border-neutral-800 flex flex-col">
        <div className="px-4 py-3 border-b border-neutral-800 flex items-center gap-2">
          <Shield size={16} className="text-neutral-400" />
          <h2 className="text-sm font-semibold text-neutral-200">Assertion Baselines</h2>
        </div>
        <div className="flex-1 overflow-auto scrollbar-thin">
          {isLoading ? (
            <div className="text-center text-neutral-500 text-sm py-8">Loading...</div>
          ) : baselines.length === 0 ? (
            <div className="text-center text-neutral-500 text-sm py-8">
              <p>No baselines yet</p>
              <p className="text-xs mt-1">Run <code className="text-cyan-400 bg-neutral-900 px-1 rounded">rewind assert baseline</code></p>
            </div>
          ) : (
            baselines.map(b => (
              <button
                key={b.id}
                onClick={() => setSelectedName(b.name)}
                className={cn(
                  'w-full text-left px-4 py-3 border-b border-neutral-800/50 transition-colors',
                  selectedName === b.name ? 'bg-neutral-800/80' : 'hover:bg-neutral-900/60'
                )}
              >
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium text-neutral-200">{b.name}</span>
                  <ChevronRight size={14} className="text-neutral-600" />
                </div>
                <div className="flex items-center gap-3 text-xs text-neutral-500 mt-1">
                  <span>{b.step_count} steps</span>
                  <span>{formatTokens(b.total_tokens)} tokens</span>
                  <span>{timeAgo(b.created_at)}</span>
                </div>
                {b.description && <p className="text-xs text-neutral-600 mt-1 truncate">{b.description}</p>}
              </button>
            ))
          )}
        </div>
      </div>

      <div className="flex-1">
        {detail ? (
          <BaselineDetailView detail={detail} />
        ) : (
          <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
            Select a baseline to view
          </div>
        )}
      </div>
    </div>
  )
}

function BaselineDetailView({ detail }: { detail: BaselineDetail }) {
  const { baseline, steps } = detail

  return (
    <div className="flex flex-col h-full">
      <div className="px-4 py-3 border-b border-neutral-800">
        <h3 className="text-sm font-semibold text-neutral-200">{baseline.name}</h3>
        <div className="flex items-center gap-4 text-xs text-neutral-500 mt-1">
          <span>{baseline.step_count} steps</span>
          <span>{formatTokens(baseline.total_tokens)} tokens</span>
          <span>{timeAgo(baseline.created_at)}</span>
        </div>
        {baseline.description && <p className="text-xs text-neutral-400 mt-1">{baseline.description}</p>}
      </div>

      <div className="flex-1 overflow-auto scrollbar-thin">
        <table className="w-full text-xs">
          <thead className="sticky top-0 bg-neutral-950">
            <tr className="text-neutral-500 border-b border-neutral-800">
              <th className="text-left px-4 py-2 font-medium">#</th>
              <th className="text-left px-4 py-2 font-medium">Type</th>
              <th className="text-left px-4 py-2 font-medium">Model</th>
              <th className="text-left px-4 py-2 font-medium">Status</th>
              <th className="text-left px-4 py-2 font-medium">Tool</th>
              <th className="text-right px-4 py-2 font-medium">Tokens</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-neutral-800/50">
            {steps.map(step => (
              <tr key={step.id} className="text-neutral-300 hover:bg-neutral-900/60">
                <td className="px-4 py-2 font-mono text-neutral-500">{step.step_number}</td>
                <td className="px-4 py-2">{step.step_type}</td>
                <td className="px-4 py-2 font-mono text-neutral-500">{step.expected_model || '-'}</td>
                <td className="px-4 py-2">
                  <span className={cn('flex items-center gap-1', step.has_error ? 'text-red-400' : 'text-green-400')}>
                    {step.has_error ? <XCircle size={12} /> : <CheckCircle2 size={12} />}
                    {step.expected_status}
                  </span>
                </td>
                <td className="px-4 py-2 text-neutral-500">{step.tool_name || '-'}</td>
                <td className="px-4 py-2 text-right text-neutral-500">{formatTokens(step.tokens_in + step.tokens_out)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}
