# Project Instructions

<!-- deciduous:start -->
## Decision Graph Workflow

**THIS IS MANDATORY. Log decisions IN REAL-TIME, not retroactively.**

### Available Slash Commands

| Command | Purpose |
|---------|---------|
| `/decision` | Manage decision graph - add nodes, link edges, sync |
| `/recover` | Recover context from decision graph on session start |
| `/work` | Start a work transaction - creates goal node before implementation |
| `/document` | Generate comprehensive documentation for a file or directory |
| `/build-test` | Build the project and run the test suite |
| `/serve-ui` | Start the decision graph web viewer |
| `/sync-graph` | Export decision graph to GitHub Pages |
| `/decision-graph` | Build a decision graph from commit history |
| `/sync` | Multi-user sync - pull events, rebuild, push |

### Available Skills

| Skill | Purpose |
|-------|---------|
| `/pulse` | Map current design as decisions (Now mode) |
| `/narratives` | Understand how the system evolved (History mode) |
| `/archaeology` | Transform narratives into queryable graph |

### The Node Flow Rule - CRITICAL

The canonical flow through the decision graph is:

```
goal -> options -> decision -> actions -> outcomes
```

- **Goals** lead to **options** (possible approaches to explore)
- **Options** lead to a **decision** (choosing which option to pursue)
- **Decisions** lead to **actions** (implementing the chosen approach)
- **Actions** lead to **outcomes** (results of the implementation)
- **Observations** attach anywhere relevant
- Goals do NOT lead directly to decisions -- there must be options first
- Options do NOT come after decisions -- options come BEFORE decisions
- Decision nodes should only be created when an option is actually chosen, not prematurely

### The Core Rule

```
BEFORE you do something -> Log what you're ABOUT to do
AFTER it succeeds/fails -> Log the outcome
CONNECT immediately -> Link every node to its parent
AUDIT regularly -> Check for missing connections
```

### Behavioral Triggers - MUST LOG WHEN:

| Trigger | Log Type | Example |
|---------|----------|---------|
| User asks for a new feature | `goal` **with -p** | "Add dark mode" |
| Exploring possible approaches | `option` | "Use Redux for state" |
| Choosing between approaches | `decision` | "Choose state management" |
| About to write/edit code | `action` | "Implementing Redux store" |
| Something worked or failed | `outcome` | "Redux integration successful" |
| Notice something interesting | `observation` | "Existing code uses hooks" |

### Document Attachments

Attach files (images, PDFs, diagrams, specs, screenshots) to decision graph nodes for rich context.

```bash
# Attach a file to a node
deciduous doc attach <node_id> <file_path>
deciduous doc attach <node_id> <file_path> -d "Architecture diagram"
deciduous doc attach <node_id> <file_path> --ai-describe

# List documents
deciduous doc list              # All documents
deciduous doc list <node_id>    # Documents for a specific node

# Manage documents
deciduous doc show <doc_id>     # Show document details
deciduous doc describe <doc_id> "Updated description"
deciduous doc describe <doc_id> --ai   # AI-generate description
deciduous doc open <doc_id>     # Open in default application
deciduous doc detach <doc_id>   # Soft-delete (recoverable)
deciduous doc gc                # Remove orphaned files from disk
```

**When to suggest document attachment:**

| Situation | Action |
|-----------|--------|
| User shares an image or screenshot | Ask: "Want me to attach this to the current goal/action node?" |
| User references an external document | Ask: "Should I attach a copy to the decision graph?" |
| Architecture diagram is discussed | Suggest attaching it to the relevant goal node |
| Files not in the project are dropped in | Attach to the most relevant active node |

**Do NOT aggressively prompt for documents.** Only suggest when files are directly relevant to a decision node. Files are stored in `.deciduous/documents/` with content-hash naming for deduplication.

### CRITICAL: Capture VERBATIM User Prompts

**Prompts must be the EXACT user message, not a summary.** When a user request triggers new work, capture their full message word-for-word.

**BAD - summaries are useless for context recovery:**
```bash
# DON'T DO THIS - this is a summary, not a prompt
deciduous add goal "Add auth" -p "User asked: add login to the app"
```

**GOOD - verbatim prompts enable full context recovery:**
```bash
# Use --prompt-stdin for multi-line prompts
deciduous add goal "Add auth" -c 90 --prompt-stdin << 'EOF'
I need to add user authentication to the app. Users should be able to sign up
with email/password, and we need OAuth support for Google and GitHub. The auth
should use JWT tokens with refresh token rotation.
EOF

# Or use the prompt command to update existing nodes
deciduous prompt 42 << 'EOF'
The full verbatim user message goes here...
EOF
```

**When to capture prompts:**
- Root `goal` nodes: YES - the FULL original request
- Major direction changes: YES - when user redirects the work
- Routine downstream nodes: NO - they inherit context via edges

**Updating prompts on existing nodes:**
```bash
deciduous prompt <node_id> "full verbatim prompt here"
cat prompt.txt | deciduous prompt <node_id>  # Multi-line from stdin
```

Prompts are viewable in the web viewer.

### CRITICAL: Maintain Connections

**The graph's value is in its CONNECTIONS, not just nodes.**

| When you create... | IMMEDIATELY link to... |
|-------------------|------------------------|
| `outcome` | The action that produced it |
| `action` | The decision that spawned it |
| `decision` | The option(s) it chose between |
| `option` | Its parent goal |
| `observation` | Related goal/action |
| `revisit` | The decision/outcome being reconsidered |

**Root `goal` nodes are the ONLY valid orphans.**

### Quick Commands

```bash
deciduous add goal "Title" -c 90 -p "User's original request"
deciduous add action "Title" -c 85
deciduous link FROM TO -r "reason"  # DO THIS IMMEDIATELY!
deciduous serve   # View live (auto-refreshes every 30s)
deciduous sync    # Export for static hosting

# Metadata flags
# -c, --confidence 0-100   Confidence level
# -p, --prompt "..."       Store the user prompt (use when semantically meaningful)
# -f, --files "a.rs,b.rs"  Associate files
# -b, --branch <name>      Git branch (auto-detected)
# --commit <hash|HEAD>     Link to git commit (use HEAD for current commit)
# --date "YYYY-MM-DD"      Backdate node (for archaeology)

# Branch filtering
deciduous nodes --branch main
deciduous nodes -b feature-auth
```

### CRITICAL: Link Commits to Actions/Outcomes

**After every git commit, link it to the decision graph!**

```bash
git commit -m "feat: add auth"
deciduous add action "Implemented auth" -c 90 --commit HEAD
deciduous link <goal_id> <action_id> -r "Implementation"
```

The `--commit HEAD` flag captures the commit hash and links it to the node. The web viewer will show commit messages, authors, and dates.

### Git History & Deployment

```bash
# Export graph AND git history for web viewer
deciduous sync

# This creates:
# - docs/graph-data.json (decision graph)
# - docs/git-history.json (commit info for linked nodes)
```

To deploy to GitHub Pages:
1. `deciduous sync` to export
2. Push to GitHub
3. Settings > Pages > Deploy from branch > /docs folder

Your graph will be live at `https://<user>.github.io/<repo>/`

### Branch-Based Grouping

Nodes are auto-tagged with the current git branch. Configure in `.deciduous/config.toml`:
```toml
[branch]
main_branches = ["main", "master"]
auto_detect = true
```

### Audit Checklist (Before Every Sync)

1. Does every **outcome** link back to what caused it?
2. Does every **action** link to why you did it?
3. Any **dangling outcomes** without parents?

### Git Staging Rules - CRITICAL

**NEVER use broad git add commands that stage everything:**
- ❌ `git add -A` - stages ALL changes including untracked files
- ❌ `git add .` - stages everything in current directory
- ❌ `git add -a` or `git commit -am` - auto-stages all tracked changes
- ❌ `git add *` - glob patterns can catch unintended files

**ALWAYS stage files explicitly by name:**
- ✅ `git add src/main.rs src/lib.rs`
- ✅ `git add Cargo.toml Cargo.lock`
- ✅ `git add .claude/commands/decision.md`

**Why this matters:**
- Prevents accidentally committing sensitive files (.env, credentials)
- Prevents committing large binaries or build artifacts
- Forces you to review exactly what you're committing
- Catches unintended changes before they enter git history

### Session Start Checklist

```bash
deciduous check-update    # Update needed? Run 'deciduous update' if yes
                          # (auto-checked every 24h if auto-update is on)
deciduous nodes           # What decisions exist?
deciduous edges           # How are they connected? Any gaps?
deciduous doc list        # Any attached documents to review?
git status                # Current state
```

### Multi-User Sync

Sync decisions with teammates via event logs:

```bash
# Check sync status
deciduous events status

# Apply teammate events (after git pull)
deciduous events rebuild

# Compact old events periodically
deciduous events checkpoint --clear-events
```

Events auto-emit on add/link/status commands. Git merges event files automatically.
<!-- deciduous:end -->
The project's purpose is to provide a clean reimplementation of macos's actool. The initial main goal is to provide the basic compilation step. It should be able to run:
```
actool --compile "test_outdir" --platform macosx --minimum-deployment-target "11.0" --app-icon AppIcon --output-partial-info-plist "test_outdir/AppIcon.Info.plist" test/Images.xcassets
```
and create an identical archive compared to the system version.

You can use /usr/bin/actool which will call the system version. Don't try to use it to read/write in /tmp and other places which may be shared - use local subdirectories.

You can create tools to help the analysis, comparison, compiling, etc. but the main app should have the cli and arguments compatible with the original.

Use Rust integration tests under `tests/` (one file per area, e.g. `packing_regressions.rs`, `dmp2_regressions.rs`) for full-flow coverage, and run `tools/validate_repos.py --only <slug>` to check results against third-party repositories.
Don't use comments that repeat the code logic. Only use comments for details about why the code is there.

## Debugging `.car` parity issues

Three complementary tools — use all three:
- `./tools/validate_car <path>` — runs CoreUI's actual `imagesWithName:` / `colorWithName:` paths. Closest signal to "does it work in a real app?" Apple's reference reports 9 OK / 1 SKIP on the element-web fixture; that's the parity target (the SKIP is the non-raster SVG layer facet — see the `SKIP (no bitmaps)` note above; match the **OK** count).
- `python3 tools/compare_car.py <a> <b>` — structural diff; now audits BOM physical layout (inline-key region, BITMAPKEYS, tree headers, named-block order) — the categories most likely to cause silent CUICatalog failures.
- `/usr/bin/actool` is the reference. Compile our output and Apple's into separate local dirs (not `/tmp/`-shared) at the same `--minimum-deployment-target` and diff.

Element-web `.icon` at `third_party/element-web/apps/desktop/build/icon.icon/` is the canonical IconComposer fixture (1 group, 1 layer, `fill: "automatic"` → 5 Color + 2 Gradient renditions). For the overall `.icon` parity status — what is byte-matched vs renderer-bound, and why byte-for-byte is impossible (Apple embeds random UUIDs in rendition names) — see `docs/icon-bundle-parity.md`. Bundles with a top-level `fill-specializations` block (feishin, scrumdinger) derive their Color/Gradient palette from `fill_specializations_assets`, not the plain-`fill` shapes.

### `.icon` (IconComposer) catalog gotchas that cost many sessions to find

These are all *silent* — the catalog loads, but `imagesWithName:` returns empty. None surface in a naive tree-walk diff.

- **BOM leaf inline-key region.** Fixed-key trees (RENDITIONS, APPEARANCEKEYS) pack the rendition keys directly after the entry table, separated by a 4-byte zero gap, then pad to total leaf size = `block_size + n × key_size`. CUICatalog reads facet→rendition mappings from this packed region. If it's zero-filled or wrong-offset, every lookup silently returns empty even when the separate key blocks are byte-perfect. See `bom.rs::build_leaf_node`.
- **CARHEADER coreUI version must be at least 975 for IconComposer.** Lower values activate the legacy path and lookups fail. `.icon` writes the detected toolchain version (1011 on Xcode 26/27) via `compile_icon_bundle_with_options`, floored at 975; the plain `compile_icon_bundle` wrapper stays on the deterministic 975 baseline for tests. Default `972` elsewhere.
- **BITMAPKEYS must list every facet identifier** with a 52-byte attribute-mask blob (`-1` for variable attrs 7/1/2/17; bitmask of seen values for the rest). See `icon_bundle::build_bitmapkeys`. An empty tree breaks every facet lookup.
- **Color/Gradient rendition KEYS use `scale=1`** even though the CSI's `scale_factor` field is 0. `colorWithName:displayGamut:` filters at the key level.
- **Pre-rendered icon PNG renditions need `template_rendering_intent: 0`** (original). The default `-1` → automatic (4) sets a flag that makes CUICatalog look for a template variant.
- **`.icon` layer assets need `force_non_opaque: true`** so CELM encodes ver=0 (non-opaque) regardless of source alpha — layers always composite with alpha inside iconstack rendering.
- **Multi-group `.icon`: every group needs its own IconGroup rendition** and the iconstack must reference all groups (back-to-front; icon.json lists them front-to-back). Wiring only `groups[0]` leaves later groups' facets in FACETKEYS but absent from BITMAPKEYS → CUICatalog returns "no images" for them. Fixing this matched Apple's rendition count exactly (Rectangle 30/30, transmission 49/49). See `icon_bundle.rs` `if layered` block. Note `validate_car` reports **group** facets (`<icon>/Group`) and **SVG layer** facets (`<icon>_Assets/<svg-stem>`) as `SKIP (no bitmaps)`, not a failure — they're non-raster structural/vector facets that `imagesWithName:` correctly returns nothing for, and Apple's reference reports the identical SKIP set. A genuine regression shows up as a drop in the **OK** count (compare to the reference), so the tool exits 0 when only SKIPs remain.
- **Frosted-glass colour is gated on `shadow: layer-color`.** A glass layer keeps its colour (multiplies the background gradient, accumulated per-pixel so overlapping tinted groups stack → purple where blue meets red) only when its group's shadow kind is `layer-color`; `none`/`neutral` strip it to a neutral relief. Verified on a synthetic two-group fixture — flipping the shadow kind is what toggles colour retention (scrumdinger=`neutral`→grey, Rectangle Overlay=`layer-color`→blue). The exact tint strength is renderer-bound.

- **BOM variable-key trees (FACETKEYS, APPEARANCEKEYS) must be compact on CoreUI 1000+.** Apple records key size `0xFFFFFFFF` and keeps the leaf at exactly one 4096-byte block with no inline-key region (`BomWriter::set_compact_variable_trees`). The padded layout still loads on macOS but makes **iOS-keyed catalogs return no images/colors at all** (`validate_car` 1 OK / 8 SKIP instead of 8 OK / 1 SKIP) even though every rendition and block is otherwise byte-identical.
- **CELM header word 4 is the KCBC chunk count**, not a constant 3. Rows are split into `height/3`-row chunks, so 256/512/1024 rows give 4 chunks (a 1-row tail); a wrong count risks truncated rows.
- **CoreUI 1000+ gradient names:** the stock light/dark background pairs (gray 1.0→0.925, 0.192→0.078) are named `system-light`/`system-dark`; others stay `Gradient-N`. Colors deduplicate by value (a white first stop is the `Color-1` anchor).
- **IconGroup layer entries** list layers back-to-front (reverse of icon.json), carry the layer's placement rect on the 1024pt canvas (offsets floor, sizes round), its opacity resolved for that rendition's appearance, and a glass flag (on unless `glass: false`). The iconstack's group entry carries translucency (enabled, value) and shadow (kind, opacity); neutral is kind 3, other kinds are unverified. Both carry a `0x3FE` TLV (count + 12 zero bytes per layer + 4).
- **The variant axis (attr 24) needs dark *fill* content.** Dark `opacity-`/`hidden-specializations` (a layer visibility swap, e.g. Chimera) do not trigger it.
- **Apple's own main-facet part is non-deterministic.** The `<icon>` facet flips between 220 and 245 on identical input (50/50 over 14 runs), so don't chase it. Re-run Apple several times before deriving a rule from a single sample (`determinism.py`-style: compare non-pixel CSI across runs).
- Glass is not frosted for colour bitmaps or when only `translucency` is on: Apple renders srb2 (`glass: true`, translucency 0.3) and Chimera crisp. Frosted relief only applies to vector (SVG) glass layers.

Other surprising behaviors:
- Apple emits the **same** `.car` at every `--minimum-deployment-target` from 11.0–26.0 — only the `deploymentPlatformVersion` string in EXTENDED_METADATA changes. No version-specific output paths.
- For `.icon` bundles Apple emits `<app-icon>.icns` **and** a populated partial plist (`CFBundleIconFile` + `CFBundleIconName`) iff the bundle's filename stem matches `--app-icon` case-sensitively. Stem mismatch → only `Assets.car` + an empty `<dict/>` plist (181 bytes). Neither `supported-platforms` nor `--standalone-icon-behavior` affects this gate (verified on `Icon.icon` vs `lc_icon.icon` and against `--standalone-icon-behavior none`). The legacy xcassets path (`compiler.rs`) still emits both.
- BOM named-block emission order: `CARHEADER, RENDITIONS, FACETKEYS, APPEARANCEKEYS, KEYFORMAT, EXTENDED_METADATA, BITMAPKEYS` (RENDITIONS early, KEYFORMAT late) — not load-blocking on its own but reduces the diff.

### iOS `.icon` bundles (`--platform iphoneos`/`iphonesimulator`)

Apple emits `<name>60x60@2x.png` (120px) and `<name>76x76@2x~ipad.png` (152px) next to `Assets.car`, no `.icns`, and the iOS partial plist (`CFBundleIcons`/`CFBundleIcons~ipad`, `CFBundleIconFiles` = `<name>60x60` / `[<name>60x60, <name>76x76]`). `CFBundleIconName` appears only for the `--target-device` values given (both when none). The CAR is the 975+ icon catalog with iOS keys: `[7,13,12,15,16,9,17,1,2]`, key semantics 2, APPEARANCEKEYS `UIAppearanceAny 0 / Dark 1 / Light 4 / ISAppearanceTintable 10`, iconstack/IconGroup at appearances 1/4/10, no atlas, and per idiom (iphone 1, ipad 2) one full-bleed 1024 BGRA composite each for Any, Dark and Tintable (grayscale on black), dim2 index 1, plus a one-entry multisize rendition. Layers and the gradient span the whole canvas (base scale 1.0, no squircle/margin/shadow). An `--app-icon` that doesn't name the bundle is a hard error (exit 1, only an empty `<dict/>` plist). The non-pixel CSI for all four `examples/` icons is byte-identical to Apple's; pixel content (shading, rim light) is renderer-bound.

### iOS platform (`--platform iphoneos`) — xcassets app icons

Implemented (`car.rs::is_idiom_platform`/`idiom_value`/`deployment_platform_name`/`KEYFORMAT_IOS`/`make_carheader_full`, `compiler.rs`, `catalog.rs::stamp_idiom`):
- Rendition keys carry **Idiom (attr 15)** + **Subtype (attr 16)**; idiom enum: universal=0, iphone/phone=1, ipad/pad=2, tv=3, car=4, watch=5, ios-marketing/marketing=6.
- Imageset key format is fixed `[7,13,12,15,16,17,1,2]` (no trimming like macOS). CARHEADER = CoreUI **975** / key-semantics **2**. EXTENDED_METADATA platform string is **"ios"** (device family, not "iphoneos"). APPEARANCEKEYS always emits `UIAppearanceAny=0`. Imagesets keep iphone/ipad/universal and drop `mac` + `ios-marketing`.
- **App icons**: emit loose `@2x` home-screen PNGs per idiom (`AppIcon60x60@2x.png`, `AppIcon76x76@2x~ipad.png` — primary sizes iphone=60/ipad=76; marketing is CAR-only) and the iOS partial plist (`CFBundleIcons`/`CFBundleIcons~ipad`, each with `CFBundleIconName` always and `CFBundleIconFiles` when the idiom ships an icon; iPad list = iPhone primary + iPad primary, populated only when iPad has an icon). No `.icns`. Icon renditions are idiom-tagged.

**App-icon CAR parity still incomplete** (reference via `/usr/bin/actool --platform iphoneos --app-icon AppIcon`). The CAR loads (`validate_car` OK) but does not byte-match.

Done: the key format now byte-matches Apple's `[7,13,12,15,16,9,8,17,1,2]`.
- **dim2(9) + dim1(8) key columns** (`car::compute_keyformat_ios` inserts 9 then 8 after Subtype — iOS order is reverse of macOS's `…8,9…`) and the **iOS size→index map** (`catalog::ios_icon_dim2` / `icon_size_index`): 20→1, 29→2, 40→3, 60→4, 76→5, 83(.5)→6, 90→7, 1024→8. dim2 fixed a real collision (same idiom+scale icons shared one key and collapsed). Multisize entries carry correct indices.
- **Icon atlas packing**: app icons pack into `ZZZZPackedAsset-*-gamut0` atlases (per-idiom; idiom threads through to the INLK link). dim1(8) appears when a scale has >1 atlas. See `docs/atlas-packing.md` for the packing rules and gotchas.

Done: **per-idiom MultiSized renditions** (`catalog::parse_appiconset` groups icon sizes by idiom and emits one MultiSized rendition per idiom, setting `idiom` on each so the key carries attr 15). MSIS entries are plain (w,h,index) triples — the idiom lives in the rendition key, not the entries. We now match phone `{20:1,29:2,40:3,60:4}`, pad `{20:1,29:2,40:3,76:5,83:6}`, marketing `{1024:8}`.

Done: **subtype-1792 Plus-phone synthesis** (`catalog::parse_appiconset`). When a 60pt@3x iPhone icon is present, actool synthesizes a 90pt@2x icon (subtype 1792) reusing that 180px source, plus a dedicated phone/subtype-1792 multisize `{90:7}`. We clone the 60pt@3x rendition (scale→2, subtype→1792, dim2→7), and the multisize grouping keys by (idiom, subtype). The synthesized leaf is forced inline (`packer`: subtype≠0 → inline). Rendition count now matches the reference exactly (29=29); matched 23/29.

Remaining — **atlas geometry only** (the CAR loads; every non-atlas rendition, key, multisize and the subtype-1792 synthesis match the reference). Functionally irrelevant (CUICatalog uses the exact INLK coordinates) and intentionally left unmatched — Apple's icon bin-packer wasn't reverse-engineerable from a 59-config sweep. Full analysis, reference layouts and the dropped-sweep dead-ends are in `docs/atlas-packing.md`.
