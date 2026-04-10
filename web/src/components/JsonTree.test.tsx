import { describe, it, expect } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { JsonTree } from './JsonTree'

describe('JsonTree', () => {
  it('renders null', () => {
    render(<JsonTree data={null} />)
    expect(screen.getByText('null')).toBeInTheDocument()
  })

  it('renders a string', () => {
    render(<JsonTree data="hello" />)
    expect(screen.getByText(/"hello"/)).toBeInTheDocument()
  })

  it('renders a number', () => {
    render(<JsonTree data={42} />)
    expect(screen.getByText('42')).toBeInTheDocument()
  })

  it('renders a boolean', () => {
    render(<JsonTree data={true} />)
    expect(screen.getByText('true')).toBeInTheDocument()
  })

  it('renders an empty array', () => {
    render(<JsonTree data={[]} />)
    expect(screen.getByText('[]')).toBeInTheDocument()
  })

  it('renders an empty object', () => {
    render(<JsonTree data={{}} />)
    expect(screen.getByText('{}')).toBeInTheDocument()
  })

  it('renders object keys at depth 0 (auto-expanded)', () => {
    render(<JsonTree data={{ name: "test", value: 123 }} />)
    expect(screen.getByText(/"name":/)).toBeInTheDocument()
    expect(screen.getByText(/"test"/)).toBeInTheDocument()
    expect(screen.getByText('123')).toBeInTheDocument()
  })

  it('renders array items at depth 0 (auto-expanded)', () => {
    render(<JsonTree data={["a", "b"]} />)
    expect(screen.getByText(/"a"/)).toBeInTheDocument()
    expect(screen.getByText(/"b"/)).toBeInTheDocument()
  })

  it('collapses deeply nested objects', () => {
    const data = { level1: { level2: { level3: { deep: "value" } } } }
    render(<JsonTree data={data} />)
    expect(screen.getByText(/"level1":/)).toBeInTheDocument()
    expect(screen.getByText(/"level2":/)).toBeInTheDocument()
    // level3 should be collapsed at depth 2
    expect(screen.queryByText(/"deep":/)).not.toBeInTheDocument()
  })

  it('shows truncation button for long strings', () => {
    const longStr = 'x'.repeat(300)
    render(<JsonTree data={longStr} />)
    expect(screen.getByText(/\+100/)).toBeInTheDocument()
  })

  it('expands long string on click', async () => {
    const user = userEvent.setup()
    const longStr = 'abc'.repeat(100)
    render(<JsonTree data={longStr} />)

    const expandBtn = screen.getByText(/\+\d+/)
    await user.click(expandBtn)
    expect(screen.getByText('less')).toBeInTheDocument()
  })
})
