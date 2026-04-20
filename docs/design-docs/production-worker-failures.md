# Production Worker Failure Analysis

Documenting specific worker failure patterns observed in production logs.

## Exact Errors Observed

### Context Length Exceeded (Primary Failure Mode)

**Error Message:**
```
OpenRouter API error (400 Bad Request): This endpoint's maximum context length is 262144 tokens. However, you requested about 1,989,253 tokens (1,988,096 of text input, 1,157 of tool input)
```

**What Happened:**
Workers accumulated tool output faster than context compaction could keep up. The shell tool returned full directory listings of directories containing thousands of files (node_modules, .next build output). File reads on large TypeScript files added thousands of tokens per read. Grep searches returned hundreds of matches. Each tool result was appended to the conversation history without truncation.

**Specific Example:**
Worker searched for SQL parameter patterns across a codebase. Found 10,666 TypeScript files. Grepped for "@limit" across all of them. Each match returned full file paths and line context. Tool output exceeded 50KB. Multiple file reads on database schema files (each 100+ lines). History grew to 1.9M tokens before the 400 error.

**Root Cause:**
No output size limits on shell/file tools. No warning at context thresholds. Workers continued operating normally while silently approaching the limit, then hit a hard wall at 262k tokens.

---

### Empty Response from Provider

**Error Message:**
```
CompletionError: ResponseError: empty response from OpenAI
```

**What Happened:**
Worker made LLM call after accumulating ~35 messages in history. Provider returned empty response body. Retry logic attempted same call twice more. Same empty response. Worker failed after 3 attempts.

**Frequency:**
~10 workers failed with this exact error pattern over 24-hour period.

**Hypothesis:**
Provider-side timeout or content filter triggered by large context + specific prompt content combination. Empty response suggests request was accepted but processing failed silently on provider side.

---

### Response Body Decoding Errors

**Error Message:**
```
CompletionError: ProviderError: failed to read response body: error decoding response body
```

**What Happened:**
Worker initiated LLM call. HTTP connection established. Response headers received. Body stream started but failed mid-stream. Decoding error suggests partial/corrupted response.

**Frequency:**
~8 workers over 48-hour period.

**Hypothesis:**
Network instability between our server and LLM provider. Large response bodies (due to large contexts) increase probability of TCP stream interruption. No retry with backoff implemented.

---

### Shell Command Output Overflow

**Observed Pattern:**
```
Tool Result (id: shell:5):
  {"success":true,"exit_code":0,"stdout":"./node_modules/pkce-challenge/dist\n./node_modules/pako/dist\n./node_modules/tinyglobby/dist\n... [5000 more lines] ...\n./node_modules/@tanstack/react-router/dist\n./node_modules/@tanstack/store/dist\n./node_modules/htmlparser2/dist"}
```

**What Happened:**
Shell `find` command traversed node_modules directory. Returned 5,000+ entries (35KB+). Entire output serialized into tool result JSON. Added to conversation history as User message. No truncation applied.

**Impact:**
Single tool call consumed ~8,000 tokens. Multiple such calls in sequence rapidly approached context limit.

**Current Mitigation:**
The shell tool now emits pre-execution `analysis` metadata with command category, risk level, duration hint, and UX flags like `collapsed_by_default` and `expects_no_output`. That lets downstream UI code collapse search/read/list output and render silent successes as `Done` without re-parsing the raw command string.

---

### Working Directory Mismatch

**Observed Pattern:**
```
Tool Result (id: shell:0):
  {"success":false,"exit_code":1,"stdout":"","stderr":"sh: line 0: cd: /home/user/Projects/AppName: No such file or directory"}

[3 turns later, same worker]

Tool Result (id: shell:3):
  {"success":true,"exit_code":0,"stdout":"total 400\ndrwxr-x---+ 58 spacedrivestudio  staff   1856 Feb 12 14:52 .\n..."}
```

**What Happened:**
Worker assumed path `/home/user/Projects/AppName`. Actually running on macOS with path `/Users/spacedrivestudio/projects/appname`. Failed 3 times before trying `ls -la` in current directory and discovering correct location.

**Impact:**
3 failed tool calls added to history (with full error messages). Delayed task completion. Workers often never recovered from initial path confusion and continued making wrong assumptions.

---

### File Read on Non-Existent Path

**Observed Pattern:**
```
Tool Result (id: file:8):
  Toolset error: ToolCallError: ToolCallError: ToolCallError: File operation failed: Failed to read file: No such file or directory (os error 2)
```

**What Happened:**
Worker attempted to read `.gitignore` at `~/projects/appname/.gitignore`. File tool resolved `~` to literal tilde instead of home directory. Path didn't exist.

**Impact:**
Error returned as tool result. Added to conversation. Worker attempted file read 3 more times with different path variations before succeeding. 4 failed attempts + error messages added ~2,000 tokens to context.

---

### Browser Tool Launch Failure

**Observed Pattern:**
```
Tool Result (id: browser:0):
  {"success":false,"error":"Browser failed to launch: Failed to create ProcessSingleton"}
```

**What Happened:**
Worker attempted to browse website for documentation. Browser tool tried to launch Chromium. ProcessSingleton lock file conflict (Chromium already running or previous instance crashed without cleanup). Browser failed to start.

**Impact:**
Worker fell back to shell + curl. Curl returned HTML page. Worker attempted to parse HTML as JSON. Failed. Error propagated. Context consumed without progress.

---

### Grep Pattern Errors

**Observed Pattern:**
```
Tool Result (id: shell:12):
  {"success":true,"exit_code":0,"stdout":"","stderr":"grep: invalid option -- t\nusage: grep [-abcdDEFGHhIiJLlMmnOopqRSsUVvwXxZz] [-A num] [-B num] [-C[num]]..."}
```

**What Happened:**
Worker constructed grep command with malformed pattern. Pattern contained `--` which grep interpreted as option terminator. Command failed but exit code was 0 (stderr output, not crash).

**Impact:**
Worker interpreted as "no matches found" and continued. Later discovered error in stderr and attempted fix. Fix required another tool call. More context consumed.

---

### Recursive Investigation Loop

**Observed Pattern (5+ hour worker):**
```
[0] User: Search for SQL in ~/app that uses @limit
[1] Assistant: Tool: find ~/app -name "*.ts" | head -20
[2] User: Result: 10,666 files found  
[3] Assistant: Tool: grep -r "@limit" ~/app --include="*.ts"
[4] User: Result: 50KB of matches across node_modules and .next/
[5] Assistant: Tool: grep -r "@limit" ~/app/src --include="*.ts"
[6] User: Result: 20 matches
[7] Assistant: Tool: read file ~/app/src/backend/db/query.ts
[8] User: Result: 200 lines of code
[9] Assistant: Tool: read file ~/app/src/backend/db/index.ts  
[10] User: Result: 150 lines of code
[11] Assistant: Tool: shell: grep -n "LIMIT" ~/app/src/backend/db/*.ts
[12] User: Result: 100 lines of grep output
[13] Assistant: Tool: file: read ~/app/src/data/schema.ts
[14] User: Result: 300 lines of schema
[15] Assistant: [Context limit exceeded error]
```

**What Happened:**
Worker started with specific task. Each answer raised new questions. Read 6+ files totaling 1000+ lines. Never synthesized findings. Never called `set_status` to report progress. Continued gathering more data until context exhausted.

**Critical Failure:**
Worker accumulated 47 messages without calling any "completion" action. Task asked to "Report which files contain these" but worker kept investigating instead of reporting.

---

### Railway CLI Connection Issues

**Observed Pattern:**
```
Tool Result (id: shell:1):
  {"success":true,"exit_code":0,"stdout":"error: unexpected argument '--deployment' found\n\nUsage: railway status [OPTIONS]\n\nFor more information, try '--help'.\n\nProject: appname\nEnvironment: production\nService: appname\n"}
```

**What Happened:**
Worker attempted to check deployment logs. Used incorrect CLI syntax. Railway CLI rejected command but exited 0 with error message in stdout. Worker parsed as success. No deployment logs retrieved.

**Impact:**
Worker couldn't diagnose deployment issues. Task stalled. Multiple retry attempts with different CLI flags. Each attempt added more context. Eventually failed on unrelated context limit.

---

### Provider-Specific Bad Request

**Error Message:**
```
CompletionError: ProviderError: OpenRouter API error (400 Bad Request): This endpoint's maximum context length is 262144 tokens. However, you requested about 291480 tokens (290323 of text input, 1157 of tool input)
```

**Pattern:**
Same as first error but with different token counts (291k vs 1.9M). Occurred on different workers with varying accumulation rates.

**Key Observation:**
All context limit failures occurred between 262k and 2M tokens requested. No graceful degradation. Hard failure at provider level.

---

## Common Patterns Across Failures

1. **Tool output never truncated** - Shell commands returning 10KB+ added to history verbatim
2. **No context monitoring** - Workers operated without knowing their own size
3. **Silent accumulation** - Errors and failures added to context without triggering alerts
4. **No synthesis checkpoint** - Workers gathered data indefinitely without reporting
5. **Path confusion** - Multiple failed attempts due to wrong working directory assumptions
6. **Provider as single point of failure** - Any network/provider error killed the worker

## Data from Logs

**Total Workers Failed:** 32 over 48-hour period
**Primary Cause:** Context length exceeded (18 workers, 56%)
**Secondary Cause:** Provider empty response (10 workers, 31%)
**Tertiary Cause:** Response decoding error (4 workers, 13%)

**Average Messages Before Failure:**
- Context limit failures: 35-50 messages
- Empty response failures: 25-40 messages  
- Decoding errors: 15-30 messages

**Average Tool Calls Per Worker:**
- Successful workers: 8-15 tool calls
- Failed workers: 20-45 tool calls (before failure)

**Context Growth Rate:**
- Typical: 5,000-15,000 tokens per tool call
- Peak: 50,000+ tokens from single shell command

## Working Hypothesis

The worker system lacks defensive limits on:
1. Tool output size (should cap at 2-5KB)
2. Context accumulation (should warn at 50%, stop at 80%)
3. Investigation depth (should force synthesis after N tool calls)
4. Provider resilience (should handle empty responses gracefully)

Workers behave like they have infinite context. They don't.
