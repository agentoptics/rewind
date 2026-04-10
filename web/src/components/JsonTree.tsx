import { useState } from 'react'
import { ChevronRight, ChevronDown } from 'lucide-react'

export function JsonTree({ data, depth = 0 }: { data: unknown; depth?: number }) {
  if (data === null) return <span className="text-neutral-500">null</span>
  if (data === undefined) return <span className="text-neutral-500">undefined</span>

  if (typeof data === 'string') {
    if (data.length > 200) {
      return <CollapsibleString value={data} />
    }
    return <span className="text-green-400">"{data}"</span>
  }
  if (typeof data === 'number') return <span className="text-cyan-400">{data}</span>
  if (typeof data === 'boolean') return <span className="text-amber-400">{String(data)}</span>

  if (Array.isArray(data)) {
    if (data.length === 0) return <span className="text-neutral-500">[]</span>
    return <CollapsibleArray data={data} depth={depth} />
  }

  if (typeof data === 'object') {
    const entries = Object.entries(data as Record<string, unknown>)
    if (entries.length === 0) return <span className="text-neutral-500">{'{}'}</span>
    return <CollapsibleObject entries={entries} depth={depth} />
  }

  return <span className="text-neutral-400">{String(data)}</span>
}

function CollapsibleString({ value }: { value: string }) {
  const [expanded, setExpanded] = useState(false)
  const display = expanded ? value : value.slice(0, 200) + '...'

  return (
    <span>
      <span className="text-green-400">"{display}"</span>
      <button
        onClick={() => setExpanded(!expanded)}
        className="text-[10px] text-neutral-500 hover:text-neutral-300 ml-1"
      >
        {expanded ? 'less' : `+${value.length - 200}`}
      </button>
    </span>
  )
}

function CollapsibleArray({ data, depth }: { data: unknown[]; depth: number }) {
  const [open, setOpen] = useState(depth < 2)

  if (!open) {
    return (
      <span>
        <button onClick={() => setOpen(true)} className="inline-flex items-center gap-0.5 text-neutral-500 hover:text-neutral-300">
          <ChevronRight size={12} />
          <span className="text-xs">Array[{data.length}]</span>
        </button>
      </span>
    )
  }

  return (
    <div>
      <button onClick={() => setOpen(false)} className="inline-flex items-center gap-0.5 text-neutral-500 hover:text-neutral-300">
        <ChevronDown size={12} />
        <span className="text-xs">[</span>
      </button>
      <div className="pl-4 border-l border-neutral-800 ml-1">
        {data.map((item, i) => (
          <div key={i} className="flex">
            <span className="text-neutral-600 text-xs mr-2 select-none">{i}:</span>
            <JsonTree data={item} depth={depth + 1} />
          </div>
        ))}
      </div>
      <span className="text-xs text-neutral-500">]</span>
    </div>
  )
}

function CollapsibleObject({ entries, depth }: { entries: [string, unknown][]; depth: number }) {
  const [open, setOpen] = useState(depth < 2)

  if (!open) {
    return (
      <span>
        <button onClick={() => setOpen(true)} className="inline-flex items-center gap-0.5 text-neutral-500 hover:text-neutral-300">
          <ChevronRight size={12} />
          <span className="text-xs">{'{'}...{'}'} ({entries.length} keys)</span>
        </button>
      </span>
    )
  }

  return (
    <div>
      <button onClick={() => setOpen(false)} className="inline-flex items-center gap-0.5 text-neutral-500 hover:text-neutral-300">
        <ChevronDown size={12} />
        <span className="text-xs">{'{'}</span>
      </button>
      <div className="pl-4 border-l border-neutral-800 ml-1">
        {entries.map(([key, value]) => (
          <div key={key} className="flex">
            <span className="text-purple-300 text-xs mr-1 shrink-0">"{key}":</span>
            <JsonTree data={value} depth={depth + 1} />
          </div>
        ))}
      </div>
      <span className="text-xs text-neutral-500">{'}'}</span>
    </div>
  )
}
