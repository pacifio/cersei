"use client"

import {
  Area,
  AreaChart,
  CartesianGrid,
  XAxis,
  YAxis,
  Tooltip,
} from "recharts"
import { ChartContainer, useChart, type ChartConfig } from "@/components/ui/chart"

const C = {
  abstract: "#22d3ee",
  codex: "#a78bfa",
  claude: "#fb923c",
  tool: "#22d3ee",
  // Python agent frameworks
  cersei: "#22d3ee",
  agno: "#10b981",
  pydantic: "#f472b6",
  crewai: "#eab308",
  langgraph: "#f97316",
}

const GRID = "#1a1a1a"
const AXIS = "#333"
const TICK = { fill: "#666", fontSize: 10 }

function Tip({ active, payload, label }: any) {
  if (!active || !payload?.length) return null
  return (
    <div className="rounded-md border border-neutral-800 bg-black px-2.5 py-1.5 text-[10px] shadow-2xl">
      {label && <div className="mb-0.5 font-medium text-neutral-300">{label}</div>}
      {payload.map((p: any, i: number) => (
        <div key={i} className="flex items-center gap-1.5">
          <div className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: p.stroke || p.fill || p.color }} />
          <span className="text-neutral-500">{p.name || p.dataKey}</span>
          <span className="ml-auto font-mono text-neutral-200">{typeof p.value === "number" ? p.value.toLocaleString() : p.value}</span>
        </div>
      ))}
    </div>
  )
}

function Legend({ items }: { items: { label: string; color: string }[] }) {
  return (
    <div className="flex items-center justify-center gap-4 pt-1">
      {items.map((i) => (
        <div key={i.label} className="flex items-center gap-1 text-[10px] text-neutral-500">
          <div className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: i.color }} />
          {i.label}
        </div>
      ))}
    </div>
  )
}

const L3 = [
  { label: "Abstract", color: C.abstract },
  { label: "Codex", color: C.codex },
  { label: "Claude Code", color: C.claude },
]

function Grad({ id, color }: { id: string; color: string }) {
  return (
    <linearGradient id={id} x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stopColor={color} stopOpacity={0.3} />
      <stop offset="100%" stopColor={color} stopOpacity={0} />
    </linearGradient>
  )
}

// Margin used by all charts — tight
const M = { left: 0, right: 30, top: 12, bottom: 0 }
const H = 180

// ─── Startup ───────────────────────────────────────────────────────────────

function StartupInner() {
  const { width } = useChart()
  const data = [
    { n: "Abstract", a: 22, x: 0, c: 0 },
    { n: "Codex", a: 0, x: 57, c: 0 },
    { n: "Claude Code", a: 0, x: 0, c: 266 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="s1" color={C.abstract} /><Grad id="s2" color={C.codex} /><Grad id="s3" color={C.claude} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="n" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis tick={TICK} tickLine={false} axisLine={false} width={32} tickFormatter={(v: number) => `${v}`} />
      <Tooltip content={<Tip />} />
      <Area type="monotone" dataKey="a" stroke={C.abstract} fill="url(#s1)" strokeWidth={2} name="Abstract (ms)" />
      <Area type="monotone" dataKey="x" stroke={C.codex} fill="url(#s2)" strokeWidth={2} name="Codex (ms)" />
      <Area type="monotone" dataKey="c" stroke={C.claude} fill="url(#s3)" strokeWidth={2} name="Claude Code (ms)" />
    </AreaChart>
  )
}

export function StartupChart() {
  return (
    <ChartContainer config={{ a: { color: C.abstract }, x: { color: C.codex }, c: { color: C.claude } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <StartupInner />
      <Legend items={L3} />
    </ChartContainer>
  )
}

// ─── RSS ───────────────────────────────────────────────────────────────────

function RSSInner() {
  const { width } = useChart()
  const data = [
    { n: "Abstract", a: 4.7, x: 0, c: 0 },
    { n: "Codex", a: 0, x: 44.7, c: 0 },
    { n: "Claude Code", a: 0, x: 0, c: 333 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="r1" color={C.abstract} /><Grad id="r2" color={C.codex} /><Grad id="r3" color={C.claude} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="n" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis tick={TICK} tickLine={false} axisLine={false} width={32} tickFormatter={(v: number) => `${v}`} />
      <Tooltip content={<Tip />} />
      <Area type="monotone" dataKey="a" stroke={C.abstract} fill="url(#r1)" strokeWidth={2} name="Abstract (MB)" />
      <Area type="monotone" dataKey="x" stroke={C.codex} fill="url(#r2)" strokeWidth={2} name="Codex (MB)" />
      <Area type="monotone" dataKey="c" stroke={C.claude} fill="url(#r3)" strokeWidth={2} name="Claude Code (MB)" />
    </AreaChart>
  )
}

export function RSSChart() {
  return (
    <ChartContainer config={{ a: { color: C.abstract }, x: { color: C.codex }, c: { color: C.claude } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <RSSInner />
      <Legend items={L3} />
    </ChartContainer>
  )
}

// ─── Memory Recall ─────────────────────────────────────────────────────────

function RecallInner() {
  const { width } = useChart()
  const data = [
    { n: "Graph", ms: 0.098 },
    { n: "Text", ms: 1.3 },
    { n: "Codex", ms: 5751 },
    { n: "Claude", ms: 7545 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs>
        <linearGradient id="gRc" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={C.claude} stopOpacity={0.4} />
          <stop offset="50%" stopColor={C.abstract} stopOpacity={0.1} />
          <stop offset="100%" stopColor={C.abstract} stopOpacity={0} />
        </linearGradient>
      </defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="n" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis
        tick={{ fill: "#666", fontSize: 9 }} tickLine={false} axisLine={false} width={38}
        scale="log" domain={[0.01, 10000]}
        tickFormatter={(v: number) => v >= 1000 ? `${(v / 1000).toFixed(0)}s` : v >= 1 ? `${v}ms` : `${(v * 1000).toFixed(0)}us`}
      />
      <Tooltip
        content={({ active, payload, label }: any) => {
          if (!active || !payload?.length) return null
          const v = payload[0].value as number
          const f = v >= 1000 ? `${(v / 1000).toFixed(1)}s` : v >= 1 ? `${v.toFixed(1)}ms` : `${(v * 1000).toFixed(0)}us`
          return <div className="rounded-md border border-neutral-800 bg-black px-2.5 py-1.5 text-[10px] shadow-2xl"><div className="text-neutral-300">{label}</div><div className="font-mono text-neutral-200">{f}</div></div>
        }}
      />
      <Area type="monotone" dataKey="ms" stroke={C.claude} fill="url(#gRc)" strokeWidth={2} dot={{ fill: C.claude, r: 2.5 }} />
    </AreaChart>
  )
}

export function RecallChart() {
  return (
    <ChartContainer config={{ ms: { color: C.claude } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <RecallInner />
      <Legend items={[{ label: "Abstract (graph)", color: C.abstract }, { label: "Abstract (text)", color: C.abstract }, { label: "Codex", color: C.codex }, { label: "Claude", color: C.claude }]} />
    </ChartContainer>
  )
}

// ─── Throughput ─────────────────────────────────────────────────────────────

function ThroughputInner() {
  const { width } = useChart()
  const data = [
    { n: "Abstract", a: 1564, x: 0, c: 0 },
    { n: "Codex", a: 0, x: 4152, c: 0 },
    { n: "Claude Code", a: 0, x: 0, c: 12079 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="t1" color={C.abstract} /><Grad id="t2" color={C.codex} /><Grad id="t3" color={C.claude} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="n" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis tick={TICK} tickLine={false} axisLine={false} width={36} tickFormatter={(v: number) => v >= 1000 ? `${(v / 1000).toFixed(0)}k` : `${v}`} />
      <Tooltip content={<Tip />} />
      <Area type="monotone" dataKey="a" stroke={C.abstract} fill="url(#t1)" strokeWidth={2} name="Abstract (ms/req)" />
      <Area type="monotone" dataKey="x" stroke={C.codex} fill="url(#t2)" strokeWidth={2} name="Codex (ms/req)" />
      <Area type="monotone" dataKey="c" stroke={C.claude} fill="url(#t3)" strokeWidth={2} name="Claude Code (ms/req)" />
    </AreaChart>
  )
}

export function ThroughputChart() {
  return (
    <ChartContainer config={{ a: { color: C.abstract }, x: { color: C.codex }, c: { color: C.claude } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <ThroughputInner />
      <Legend items={L3} />
    </ChartContainer>
  )
}

// ─── Tool Dispatch ─────────────────────────────────────────────────────────

function ToolDispatchInner() {
  const { width } = useChart()
  const data = [
    { t: "Edit", ms: 0.02 },
    { t: "Glob", ms: 0.05 },
    { t: "Write", ms: 0.06 },
    { t: "Read", ms: 0.09 },
    { t: "Grep", ms: 6.04 },
    { t: "Bash", ms: 16.67 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="gTl" color={C.tool} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="t" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis tick={TICK} tickLine={false} axisLine={false} width={28} />
      <Tooltip content={<Tip />} />
      <Area type="monotone" dataKey="ms" stroke={C.tool} fill="url(#gTl)" strokeWidth={2} dot={{ fill: C.tool, r: 2.5 }} name="ms" />
    </AreaChart>
  )
}

export function ToolDispatchChart() {
  return (
    <ChartContainer config={{ ms: { color: C.tool } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <ToolDispatchInner />
      <Legend items={[{ label: "Cersei SDK dispatch (ms)", color: C.tool }]} />
    </ChartContainer>
  )
}

// ─── Graph ON vs OFF ───────────────────────────────────────────────────────

function GraphInner() {
  const { width } = useChart()
  const data = [
    { op: "Scan", off: 1310, on: 1308 },
    { op: "Recall", off: 1359, on: 103 },
    { op: "Context", off: 17, on: 16 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="gO" color={C.claude} /><Grad id="gN" color={C.abstract} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="op" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis tick={TICK} tickLine={false} axisLine={false} width={32} tickFormatter={(v: number) => v >= 1000 ? `${(v / 1000).toFixed(1)}k` : `${v}`} />
      <Tooltip content={<Tip />} />
      <Area type="monotone" dataKey="off" stroke={C.claude} fill="url(#gO)" strokeWidth={2} name="OFF (us)" dot={{ fill: C.claude, r: 2.5 }} />
      <Area type="monotone" dataKey="on" stroke={C.abstract} fill="url(#gN)" strokeWidth={2} name="ON (us)" dot={{ fill: C.abstract, r: 2.5 }} />
    </AreaChart>
  )
}

export function GraphComparisonChart() {
  return (
    <ChartContainer config={{ off: { color: C.claude }, on: { color: C.abstract } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <GraphInner />
      <Legend items={[{ label: "Graph OFF", color: C.claude }, { label: "Graph ON", color: C.abstract }]} />
    </ChartContainer>
  )
}

// ─── General-Agent Framework: Instantiation ────────────────────────────────
//
// Cersei figure is measured by
// crates/cersei-agent/benchmarks/general_agent_bench.rs.
// Python figures are from https://docs.agno.com/performance (vendor-reported,
// Apple M4 Oct 2025). Reproduce end-to-end via bench/general-agents/run.sh.

const L5 = [
  { label: "Cersei",     color: C.cersei },
  { label: "Agno",       color: C.agno },
  { label: "PydanticAI", color: C.pydantic },
  { label: "CrewAI",     color: C.crewai },
  { label: "LangGraph",  color: C.langgraph },
]

function AgentInstantiationInner() {
  const { width } = useChart()
  // μs per Agent.build() — all measured on Apple M1 Pro via the bench suite.
  const data = [
    { n: "Cersei",     v: 7.12 },      // cersei 0.1.6-patch.2
    { n: "Agno",       v: 6.50 },      // agno 2.5.17
    { n: "PydanticAI", v: 219.12 },    // pydantic-ai 1.22.0
    { n: "LangGraph",  v: 5536.17 },   // langgraph 1.1.8
    { n: "CrewAI",     v: 28508.83 },  // crewai 1.14.2
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="gai" color={C.cersei} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="n" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis
        tick={TICK} tickLine={false} axisLine={false} width={48}
        scale="log" domain={[1, 50000]}
        tickFormatter={(v: number) => v >= 1000 ? `${(v / 1000).toFixed(0)}ms` : `${v}μs`}
      />
      <Tooltip
        content={({ active, payload, label }: any) => {
          if (!active || !payload?.length) return null
          const v = payload[0].value as number
          const f = v >= 1000 ? `${(v / 1000).toFixed(2)}ms` : `${v.toFixed(2)}μs`
          return <div className="rounded-md border border-neutral-800 bg-black px-2.5 py-1.5 text-[10px] shadow-2xl"><div className="text-neutral-300">{label}</div><div className="font-mono text-neutral-200">{f}</div></div>
        }}
      />
      <Area type="monotone" dataKey="v" stroke={C.cersei} fill="url(#gai)" strokeWidth={2} dot={{ fill: C.cersei, r: 3 }} />
    </AreaChart>
  )
}

export function AgentInstantiationChart() {
  return (
    <ChartContainer config={{ v: { color: C.cersei } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <AgentInstantiationInner />
      <Legend items={[{ label: "All measured on Apple M1 Pro via bench/general-agents/run.sh", color: C.cersei }]} />
    </ChartContainer>
  )
}

// ─── General-Agent Framework: Per-agent memory ─────────────────────────────

function PerAgentMemoryInner() {
  const { width } = useChart()
  // bytes per agent — all measured on Apple M1 Pro.
  // Cersei: jemalloc stats::allocated delta / N.
  // Python: tracemalloc size_diff / N.
  const data = [
    { n: "Cersei",     v: 704 },       // cersei 0.1.6-patch.2
    { n: "Agno",       v: 5938 },      // agno 2.5.17
    { n: "PydanticAI", v: 8892 },      // pydantic-ai 1.22.0
    { n: "CrewAI",     v: 18157 },     // crewai 1.14.2
    { n: "LangGraph",  v: 30910 },     // langgraph 1.1.8
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs><Grad id="gmem" color={C.cersei} /></defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis dataKey="n" tick={TICK} tickLine={false} axisLine={false} />
      <YAxis
        tick={TICK} tickLine={false} axisLine={false} width={48}
        scale="log" domain={[100, 200000]}
        tickFormatter={(v: number) => v >= 1024 ? `${(v / 1024).toFixed(0)}KB` : `${v}B`}
      />
      <Tooltip
        content={({ active, payload, label }: any) => {
          if (!active || !payload?.length) return null
          const v = payload[0].value as number
          const f = v >= 1024 ? `${(v / 1024).toFixed(2)} KiB` : `${v} B`
          return <div className="rounded-md border border-neutral-800 bg-black px-2.5 py-1.5 text-[10px] shadow-2xl"><div className="text-neutral-300">{label}</div><div className="font-mono text-neutral-200">{f}</div></div>
        }}
      />
      <Area type="monotone" dataKey="v" stroke={C.cersei} fill="url(#gmem)" strokeWidth={2} dot={{ fill: C.cersei, r: 3 }} />
    </AreaChart>
  )
}

export function PerAgentMemoryChart() {
  return (
    <ChartContainer config={{ v: { color: C.cersei } }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <PerAgentMemoryInner />
      <Legend items={[{ label: "All measured on Apple M1 Pro. Cersei via jemalloc; Python via tracemalloc.", color: C.cersei }]} />
    </ChartContainer>
  )
}

// ─── General-Agent Framework: Max concurrent (RSS vs agent count) ──────────
//
// Cersei line is measured. Python lines will land once bench/general-agents/run.sh
// has been executed in a matched environment.

function MaxConcurrentInner() {
  const { width } = useChart()
  // Total RSS in MB after building N agents concurrently and holding them
  // live. All measured on Apple M1 Pro. Python frameworks sampled at [100,
  // 500] — pushing further on CrewAI in particular would take 10+ minutes
  // per step.
  const data = [
    { n: 100,   cersei: 8.3,  agno: 79.3,  pydantic_ai: 122.0, langgraph: 193.5, crewai: 1739.3 },
    { n: 500,   cersei: 8.5,  agno: 82.0,  pydantic_ai: 123.2, langgraph: 193.5, crewai: 1739.3 },
    { n: 1000,  cersei: 9.3 },
    { n: 5000,  cersei: 14.0 },
    { n: 10000, cersei: 22.4 },
  ]
  return (
    <AreaChart data={data} width={width} height={H} margin={M}>
      <defs>
        <Grad id="gmc1" color={C.cersei} />
        <Grad id="gmc2" color={C.agno} />
        <Grad id="gmc3" color={C.pydantic} />
        <Grad id="gmc4" color={C.langgraph} />
        <Grad id="gmc5" color={C.crewai} />
      </defs>
      <CartesianGrid strokeDasharray="3 3" stroke={GRID} />
      <XAxis
        dataKey="n" tick={TICK} tickLine={false} axisLine={false}
        scale="log" domain={[100, 10000]} type="number"
        ticks={[100, 500, 1000, 5000, 10000]}
        tickFormatter={(v: number) => v >= 1000 ? `${(v / 1000).toFixed(0)}k` : `${v}`}
      />
      <YAxis
        tick={TICK} tickLine={false} axisLine={false} width={48}
        scale="log" domain={[5, 2000]}
        tickFormatter={(v: number) => v >= 1000 ? `${(v / 1000).toFixed(1)}GB` : `${v}MB`}
      />
      <Tooltip
        content={({ active, payload, label }: any) => {
          if (!active || !payload?.length) return null
          return (
            <div className="rounded-md border border-neutral-800 bg-black px-2.5 py-1.5 text-[10px] shadow-2xl">
              <div className="mb-0.5 font-medium text-neutral-300">{label} live agents</div>
              {payload.map((p: any, i: number) => (
                <div key={i} className="flex items-center gap-1.5">
                  <div className="h-1.5 w-1.5 rounded-full" style={{ backgroundColor: p.stroke }} />
                  <span className="text-neutral-500">{p.name}</span>
                  <span className="ml-auto font-mono text-neutral-200">
                    {p.value >= 1000 ? `${(p.value / 1000).toFixed(2)} GB` : `${p.value.toFixed(1)} MB`}
                  </span>
                </div>
              ))}
            </div>
          )
        }}
      />
      <Area type="monotone" dataKey="crewai"      stroke={C.crewai}    fill="url(#gmc5)" strokeWidth={1.5} dot={{ fill: C.crewai, r: 2 }}    name="CrewAI" connectNulls />
      <Area type="monotone" dataKey="langgraph"   stroke={C.langgraph} fill="url(#gmc4)" strokeWidth={1.5} dot={{ fill: C.langgraph, r: 2 }} name="LangGraph" connectNulls />
      <Area type="monotone" dataKey="pydantic_ai" stroke={C.pydantic}  fill="url(#gmc3)" strokeWidth={1.5} dot={{ fill: C.pydantic, r: 2 }}  name="PydanticAI" connectNulls />
      <Area type="monotone" dataKey="agno"        stroke={C.agno}      fill="url(#gmc2)" strokeWidth={1.5} dot={{ fill: C.agno, r: 2 }}      name="Agno" connectNulls />
      <Area type="monotone" dataKey="cersei"      stroke={C.cersei}    fill="url(#gmc1)" strokeWidth={2}   dot={{ fill: C.cersei, r: 3 }}    name="Cersei" connectNulls />
    </AreaChart>
  )
}

export function MaxConcurrentChart() {
  return (
    <ChartContainer config={{
      cersei: { color: C.cersei }, agno: { color: C.agno }, pydantic_ai: { color: C.pydantic },
      langgraph: { color: C.langgraph }, crewai: { color: C.crewai },
    }} className="w-full my-3 rounded-lg border border-neutral-800 bg-black px-2 pt-2 pb-1" height={H + 24}>
      <MaxConcurrentInner />
      <Legend items={L5} />
    </ChartContainer>
  )
}

