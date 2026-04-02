import defaultMdxComponents from 'fumadocs-ui/mdx';
import type { MDXComponents } from 'mdx/types';
import {
  StartupChart,
  RSSChart,
  RecallChart,
  ThroughputChart,
  ToolDispatchChart,
  GraphComparisonChart,
} from './benchmark-charts';

export function getMDXComponents(components?: MDXComponents) {
  return {
    ...defaultMdxComponents,
    StartupChart,
    RSSChart,
    RecallChart,
    ThroughputChart,
    ToolDispatchChart,
    GraphComparisonChart,
    ...components,
  } satisfies MDXComponents;
}

export const useMDXComponents = getMDXComponents;

declare global {
  type MDXProvidedComponents = ReturnType<typeof getMDXComponents>;
}
