import { describe, it, expect, vi, beforeEach } from 'vitest'
import { api } from './api'

const mockFetch = vi.fn()
vi.stubGlobal('fetch', mockFetch)

function mockJsonResponse(data: unknown, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: () => Promise.resolve(data),
    text: () => Promise.resolve(JSON.stringify(data)),
  }
}

beforeEach(() => {
  mockFetch.mockReset()
})

describe('api.health', () => {
  it('calls /api/health and returns parsed JSON', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse({ status: 'ok', version: '0.2.0' }))
    const result = await api.health()
    expect(mockFetch).toHaveBeenCalledWith('/api/health')
    expect(result).toEqual({ status: 'ok', version: '0.2.0' })
  })
})

describe('api.sessions', () => {
  it('calls /api/sessions', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    const result = await api.sessions()
    expect(mockFetch).toHaveBeenCalledWith('/api/sessions')
    expect(result).toEqual([])
  })
})

describe('api.session', () => {
  it('calls /api/sessions/:id', async () => {
    const data = { session: { id: 'abc' }, timelines: [] }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.session('abc')
    expect(mockFetch).toHaveBeenCalledWith('/api/sessions/abc')
    expect(result).toEqual(data)
  })
})

describe('api.sessionSteps', () => {
  it('calls without timeline param', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    await api.sessionSteps('abc')
    expect(mockFetch).toHaveBeenCalledWith('/api/sessions/abc/steps')
  })

  it('includes timeline query param', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    await api.sessionSteps('abc', 'main')
    expect(mockFetch).toHaveBeenCalledWith('/api/sessions/abc/steps?timeline=main')
  })
})

describe('api.stepDetail', () => {
  it('calls /api/steps/:id', async () => {
    const data = { id: 'step1', step_number: 1 }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.stepDetail('step1')
    expect(mockFetch).toHaveBeenCalledWith('/api/steps/step1')
    expect(result).toEqual(data)
  })
})

describe('api.diffTimelines', () => {
  it('calls with left and right params', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse({ step_diffs: [] }))
    await api.diffTimelines('sess1', 'left-id', 'right-id')
    expect(mockFetch).toHaveBeenCalledWith(
      '/api/sessions/sess1/diff?left=left-id&right=right-id'
    )
  })
})

describe('api.baselines', () => {
  it('calls /api/baselines', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    const result = await api.baselines()
    expect(mockFetch).toHaveBeenCalledWith('/api/baselines')
    expect(result).toEqual([])
  })
})

describe('api.baseline', () => {
  it('calls /api/baselines/:name', async () => {
    const data = { baseline: { name: 'test' }, steps: [] }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.baseline('test')
    expect(mockFetch).toHaveBeenCalledWith('/api/baselines/test')
    expect(result).toEqual(data)
  })
})

describe('api.cacheStats', () => {
  it('calls /api/cache/stats', async () => {
    const data = { entries: 5, total_hits: 10, total_tokens_saved: 1000 }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.cacheStats()
    expect(mockFetch).toHaveBeenCalledWith('/api/cache/stats')
    expect(result).toEqual(data)
  })
})

describe('api.snapshots', () => {
  it('calls /api/snapshots', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    const result = await api.snapshots()
    expect(mockFetch).toHaveBeenCalledWith('/api/snapshots')
    expect(result).toEqual([])
  })
})

describe('error handling', () => {
  it('throws on non-OK response', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse('Not found', 404))
    await expect(api.sessions()).rejects.toThrow('API error 404')
  })

  it('throws on 500 response', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse('Internal error', 500))
    await expect(api.health()).rejects.toThrow('API error 500')
  })
})
