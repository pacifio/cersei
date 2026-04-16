"""
Abstract agent adapter for terminal-bench 2.0 (harbor framework).

Usage (no harbor patching required):
    cd bench && uv sync
    OPENAI_API_KEY=<key> PYTHONPATH=. uv run harbor run \
        --agent-import-path abstract_tbench:AbstractAgent \
        --model openai/gpt-5.4-2026-03-05 \
        --dataset terminal-bench@2.0 \
        --n-concurrent 8

To rebuild the Linux binary:
    docker run --rm -v $(pwd)/..:/src -w /src rust:1.94-bookworm bash -c "
      apt-get update -qq && apt-get install -y -qq pkg-config libssl-dev git >/dev/null 2>&1
      CARGO_TARGET_DIR=/tmp/tgt cargo build --release -p abstract-cli
      cp /tmp/tgt/release/abstract /src/bench/abstract-linux-arm64
    "
"""

import os
import shlex
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, EnvVar, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.trial.paths import EnvironmentPaths


# Pre-built static binaries (lives next to this file)
_BENCH_DIR = Path(__file__).resolve().parent
_BINARY_ARM64 = _BENCH_DIR / "abstract-linux-arm64"
_BINARY_AMD64 = _BENCH_DIR / "abstract-linux-amd64"


class AbstractAgent(BaseInstalledAgent):
    """
    Abstract CLI — a high-performance Rust-native coding agent.

    Copies a pre-built Linux binary into the container for instant setup (~2s).
    Supports any provider/model via --model flag.
    """

    def __init__(self, *args, enable_embedding: bool = False, **kwargs):
        self._enable_embedding = enable_embedding
        super().__init__(*args, **kwargs)

    ENV_VARS = [
        EnvVar("google_api_key", env="GOOGLE_API_KEY", env_fallback="GOOGLE_API_KEY"),
        EnvVar("gemini_api_key", env="GEMINI_API_KEY", env_fallback="GEMINI_API_KEY"),
        EnvVar("anthropic_api_key", env="ANTHROPIC_API_KEY", env_fallback="ANTHROPIC_API_KEY"),
        EnvVar("openai_api_key", env="OPENAI_API_KEY", env_fallback="OPENAI_API_KEY"),
        EnvVar("groq_api_key", env="GROQ_API_KEY", env_fallback="GROQ_API_KEY"),
        EnvVar("deepseek_api_key", env="DEEPSEEK_API_KEY", env_fallback="DEEPSEEK_API_KEY"),
        EnvVar("mistral_api_key", env="MISTRAL_API_KEY", env_fallback="MISTRAL_API_KEY"),
        EnvVar("xai_api_key", env="XAI_API_KEY", env_fallback="XAI_API_KEY"),
        EnvVar("together_api_key", env="TOGETHER_API_KEY", env_fallback="TOGETHER_API_KEY"),
        EnvVar("fireworks_api_key", env="FIREWORKS_API_KEY", env_fallback="FIREWORKS_API_KEY"),
        EnvVar("cohere_api_key", env="COHERE_API_KEY", env_fallback="COHERE_API_KEY"),
        EnvVar("openrouter_api_key", env="OPENROUTER_API_KEY", env_fallback="OPENROUTER_API_KEY"),
    ]

    @staticmethod
    def name() -> str:
        return "abstract"

    def get_version_command(self) -> str | None:
        return "abstract --version"

    def parse_version(self, stdout: str) -> str:
        # "abstract 0.1.6" -> "0.1.6"
        return stdout.strip().removeprefix("abstract").strip()

    async def install(self, environment: BaseEnvironment) -> None:
        """Upload pre-built static binary matching container architecture."""
        # Detect container architecture
        result = await environment.exec(command="uname -m")
        arch = result.stdout.strip() if result.stdout else ""

        if "x86_64" in arch or "amd64" in arch:
            binary_path = _BINARY_AMD64
        else:
            binary_path = _BINARY_ARM64

        if not binary_path.exists():
            raise RuntimeError(
                f"Binary not found at {binary_path}. "
                "See TERMINAL_BENCH.md for build instructions."
            )

        await environment.upload_file(
            source_path=binary_path,
            target_path="/usr/local/bin/abstract",
        )
        await self.exec_as_root(
            environment,
            command="chmod +x /usr/local/bin/abstract",
        )

    @with_prompt_template
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        escaped = shlex.quote(instruction)

        model_flag = ""
        if self.model_name:
            model_flag = f"--model {self.model_name} "

        embedding_flag = "--embedding-api " if self._enable_embedding else ""

        env = self.resolve_env_vars()

        # Inject learned failure patterns if available
        patterns_file = _BENCH_DIR / "failure_patterns.txt"
        if patterns_file.exists():
            patterns = patterns_file.read_text()
            # Filter out comments and empty lines, take first 20 patterns
            active_patterns = [
                line.strip() for line in patterns.splitlines()
                if line.strip() and not line.strip().startswith("#")
            ][:20]
            if active_patterns:
                env["ABSTRACT_FAILURE_PATTERNS"] = "\n".join(active_patterns)

        output_path = EnvironmentPaths.agent_dir / "abstract-output.jsonl"

        await self.exec_as_agent(
            environment,
            command=(
                f"abstract -p {escaped} "
                f"{model_flag}"
                f"{embedding_flag}"
                "--no-permissions "
                "--headless "
                "--output-format stream-json "
                f"2>&1 | tee {output_path}"
            ),
            env=env,
        )

    def populate_context_post_run(self, context: AgentContext) -> None:
        """Parse NDJSON output for token/cost info."""
        output_file = self.logs_dir / "abstract-output.jsonl"
        if not output_file.exists():
            return

        try:
            import json
            for line in output_file.read_text().splitlines():
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                    if event.get("type") == "cost_update":
                        context.n_input_tokens = event.get("input_tokens", 0)
                        context.n_output_tokens = event.get("output_tokens", 0)
                        context.cost_usd = event.get("cumulative_cost", 0.0)
                except json.JSONDecodeError:
                    continue
        except Exception:
            pass
