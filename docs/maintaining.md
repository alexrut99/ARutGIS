# Maintaining RustGIS

This page is for the [maintainers](https://github.com/orgs/opengeos/teams/rustgis-maintainers):
who approves a coordinate-first change, what happens when a proposal gets no reply, and what
reviewers check on saved files and dependencies. Contributors need only
[CONTRIBUTING.md](https://github.com/opengeos/RustGIS/blob/main/CONTRIBUTING.md).

## Who approves a coordinate-first change

CONTRIBUTING.md asks some changes to wait for a maintainer's reply. Who gives it depends on how
hard the change is to undo.

**Any maintainer**, for changes that only add. A maintainer can open the pull request without
proposing first; propose first when you're unsure which kind of change it is, or when the work is
big enough that a "no" would waste it. Say in the pull request which kind it is.

- an optional command parameter, whose default keeps today's behaviour;
- a field in a command's output;
- an optional field in the project file or another saved file, whose absence means today's
  behaviour, and whose loss when an older RustGIS saves the file costs a setting rather than data
  or a different result;
- a feature of an existing dependency, a dev-dependency or an xtask-only crate, unless it brings in
  a large set of new dependencies.

**Two maintainers**, for changes that break something that can still be put right, and for most
new dependencies. A maintainer who proposes the change counts as one of them.

- removing or renaming a command or parameter, or changing what one means, returns or defaults to;
- a new renderer, symbology or layer kind;
- a new project format version for a feature, following [saved files](#saved-files-and-settings);
- an addition to the plugin interface that plugins declare through `minRustgisVersion`;
- any other new dependency, answering the questions under [new dependencies](#new-dependencies).

**The project lead, [@giswqs](https://github.com/giswqs)**, has the final say on:

- the shape of the core types: `Coord`, `Feature`, geometries and field types;
- how a project stores its data;
- a dependency that is a C, C++ or system library, brings in a large tree such as `tokio`, or
  replaces or overlaps code RustGIS already has;
- a new crate in the workspace;
- a plugin ABI change, or removing something from the plugin interface;
- a new top-level ribbon tab;
- _Exploratory_ roadmap items, and anything that changes what RustGIS is for.

The lead also settles a disagreement between maintainers. When you approve, say which kind of
change it is, so the reply can be found later.

## When a proposal gets no reply

A contributor's proposal waits for the approval its kind needs. If a week passes without a reply,
mention @opengeos/rustgis-maintainers on the issue once.

If a maintainer proposes a change any maintainer could approve and nobody objects within a week of
the proposal or its last change, they can go ahead. An objection says what would go wrong, so that
it can be answered or the plan changed. Silence doesn't approve the other two kinds.

For a change the lead decides, ask once, on the issue: the question in a sentence, the options and
your recommendation. Link that comment from other threads rather than asking again.

## Saved files and settings

Projects, map, layer and style files, models, saved connections and sources, plugin state and
preferences outlive the release that wrote them, and people run an older and a newer RustGIS on the
same files. New and changed code follows these rules; not every existing reader does yet.

- A reader checks the version it is given.
- An older RustGIS refuses a file or setting newer than it understands, or keeps it unchanged. It
  doesn't save it again without what it couldn't read.
- A format's version goes up only when a file uses something an older reader would lose or
  misread. `project::FORMAT` is the format a project is written as, and `project::READ_FORMAT`
  the newest this version reads:
  - 1: everything up to ARutGIS 0.6.1 (RustGIS 0.6).
  - 2 (ARutGIS 0.7.0, RustGIS 0.7): what a project uses that an older reader would lose is named
    in its `requires` (`project::CAPABILITIES`), and a reader refuses a project that requires
    something it doesn't know. A new addition of that kind gets a name there; the format stays 2.
    ARutGIS's own is `frame-layers`: a layout's map frame leaves layers of its map out
    (`exclude`). RustGIS doesn't know it, so it refuses such a project by that name. `scene-3d`
    (0.7.1) is a map's 3D scene; `scene-ruler` (0.7.2) that scene's Height Ruler turned off;
    `scene-depth-exaggeration` (0.7.3) what is under its lowest ground stretched;
    `scene-ruler-step` (0.7.4) its Height Ruler marked at a step asked; `scene-ruler-range`
    (0.7.5) that ruler between heights asked, or its marks counted from one; `frame-labels`
    (0.7.6) a layout's map frame that labels a layer, or leaves its labels out, whatever its map
    does. A layout file holding such a frame is written as format 4 (3 otherwise), which 0.7.0
    to 0.7.5 refuse.
  - ARutGIS 0.6.2 and 0.6.3 wrote format 2 for a project with such a frame, and 1 otherwise,
    with no `requires`. They open a project ARutGIS 0.7.0 saved without looking at what it
    requires.
  - A layout file (`.rgislayout`) has a format of its own (`layout::FILE_FORMAT`), read up to
    it: 3 from ARutGIS 0.7.0 on (`layout::PLAIN_FILE_FORMAT`), since 0.6.3 and older would read
    past what 0.7.0 saves (a dashed outline, a legend's labels) and save the layout without it;
    4 from 0.7.6 on for a layout whose map frame chooses its layers' labels
    (`Layout::file_format`), which 0.7.0 to 0.7.5 would read past, and 3 otherwise. 0.6.2
    and 0.6.3 wrote 2 for a layout with a frame leaving layers out and no format otherwise, and
    refuse one past 2. Versions before 0.6.2 read a layout file without looking at its format,
    so a file holds its layout one level down (`Layout::to_file`): they find no layout in it
    and refuse it. RustGIS writes the layout itself, with no format: ARutGIS reads that (one
    that says a format is checked against `READ_FORMAT`, as RustGIS's other files are), and
    RustGIS refuses ARutGIS's.
- Where a change allows it, reading a new version ships at least one release before anything
  writes it.
- A project, map or layer file, a model or a web tool lists in `requires` each capability it needs
  that an older reader would lose or misread: a reader refuses one it doesn't know, naming it.
  Task, presentation and notebook readers, and models inside a task's steps, don't check
  `requires` yet. A release reads
  `.rgis` format 2 before any writes it (`READ_FORMAT` and `CAPABILITIES` in
  `crates/engine/src/project.rs`), so what is added to format 2 after that comes with a capability
  name, added to `CAPABILITIES` where it is read.
- `schema/project.schema.json` is the JSON Schema of the project file, generated from the types
  it is saved from by `cargo xtask schema`. A pull request that changes what a project saves
  regenerates it and commits it in the same pull request; CI's `cargo xtask schema --check` fails
  otherwise. A pull request that changes how it is read adds or updates a fixture project in
  `schema/fixtures/`, which must open and validate against the schema. A schema change alone
  doesn't raise the format: the rules above say when that happens.
- RustGIS releases, file formats, the plugin ABI and the GeoLibre runner protocol are versioned
  separately.
- A program RustGIS installs from elsewhere, such as the native GeoLibre runner, is pinned to a
  known release and moved forward by a pull request.

Keep historical projects and preferences in the
[release fixtures](../crates/engine/testdata/releases/README.md). These are the original bytes
written by the release named in each filename: never regenerate, reformat or resave them.
Each release adds a small project and preferences file written by that release and extends
`released_projects_and_preferences_still_open`. Record a skip in the fixtures' README when a
release has no such file format. Current-writer examples do not replace these historical files.

New project saves and map/layer exports write format 2. Their `requires` header names only
features present in the saved payload: `terrain-block`, `terrain-water`,
`shaded-relief-effects` (cast shadows, sky visibility, fill light or rock colour) and
`normalized-legend-urls` for normalized legend addresses. Project
requirements include offline map snapshots. Earlier format-2 readers refuse unknown required
capabilities instead of opening and later saving away those settings; legacy format-1 files
still open.

## New dependencies

A proposal for a dependency answers:

- Does it build for `wasm32-unknown-unknown`, or does it sit behind a native-only feature?
- Does `deny.toml` allow its licence?
- What does it add to build time and package size, measured, with only the features we need?
- What does it do better than the code we have, shown by tests (accuracy, malformed input, the
  browser)?

## The release quality pass

One maintainer takes the [release quality pass](contributing.md#releases) for each release and
says so in the version-bump pull request. Each point left for the pass is fixed, filed as an issue,
or dropped with a reply on its comment; none carries into a second release unrecorded.
