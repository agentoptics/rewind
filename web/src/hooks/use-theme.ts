import { create } from 'zustand'

type Theme = 'dark' | 'light'

interface ThemeState {
  theme: Theme
  toggle: () => void
}

export const useTheme = create<ThemeState>((set) => {
  const stored = localStorage.getItem('rewind-theme') as Theme | null
  const initial: Theme = stored || 'dark'
  document.documentElement.classList.toggle('dark', initial === 'dark')
  document.documentElement.classList.toggle('light', initial === 'light')

  return {
    theme: initial,
    toggle: () => set((s) => {
      const next: Theme = s.theme === 'dark' ? 'light' : 'dark'
      localStorage.setItem('rewind-theme', next)
      document.documentElement.classList.toggle('dark', next === 'dark')
      document.documentElement.classList.toggle('light', next === 'light')
      return { theme: next }
    }),
  }
})
