"use client"

import * as React from "react"

export type ChartConfig = Record<
  string,
  {
    label?: React.ReactNode
    color?: string
  }
>

type ChartContextProps = { config: ChartConfig; width: number }
const ChartContext = React.createContext<ChartContextProps>({ config: {}, width: 0 })

export function useChart() {
  return React.useContext(ChartContext)
}

/**
 * Chart wrapper that measures its own width via ResizeObserver
 * and provides it to children via context.
 */
export function ChartContainer({
  config,
  children,
  className,
  height = 200,
  ...props
}: {
  config: ChartConfig
  children: React.ReactNode
  className?: string
  height?: number
} & Omit<React.ComponentProps<"div">, "children">) {
  const ref = React.useRef<HTMLDivElement>(null)
  const [width, setWidth] = React.useState(0)

  React.useEffect(() => {
    if (!ref.current) return
    const el = ref.current

    const measure = () => {
      const w = el.getBoundingClientRect().width
      if (w > 0) setWidth(w)
    }

    measure()

    const observer = new ResizeObserver(() => measure())
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  return (
    <ChartContext.Provider value={{ config, width }}>
      <div
        ref={ref}
        className={className}
        style={{
          minHeight: height,
          ...Object.entries(config).reduce(
            (acc, [key, value]) => {
              if (value.color) acc[`--color-${key}`] = value.color
              return acc
            },
            {} as Record<string, string>,
          ),
        } as React.CSSProperties}
        {...props}
      >
        {width > 0 ? children : null}
      </div>
    </ChartContext.Provider>
  )
}
