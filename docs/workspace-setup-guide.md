# Workspace Setup: Athenaeum + rustafits

## Overview

Цель: единый monorepo workspace для разработки, при этом rustafits сохраняет отдельный GitHub-репозиторий (github.com/eg013ra1n/rustafits) для open-source публикации.

### Текущее состояние

- `athenaeum/` — Tauri проект (предположительно с `src-tauri/`)
- `rustafits/` — отдельный репозиторий на GitHub, со своим `.claude/` (skills, AGENTS.md)
- Skills и agent config сейчас живут в директории проекта rustafits

### Целевое состояние

- Единый workspace: `cargo test --workspace` тестирует всё
- rustafits — git submodule внутри athenaeum
- Skills и AGENTS.md — в корне athenaeum (покрывают оба crate)
- CI/CD: push в rustafits внутри workspace → автоматический sync в github.com/eg013ra1n/rustafits
- Отдельный CI для rustafits repo (crates.io publish, standalone тесты)

---

## Шаг 1: Подготовка — сохранить skills и agents из rustafits

Перед тем как rustafits станет submodule, скопируй его конфигурацию агента.

```bash
# Из директории текущего rustafits проекта
cd /path/to/current/rustafits

# Посмотри что есть
ls -la .claude/
# Ожидаем: skills/, AGENTS.md, и т.д.

# Скопируй во временную директорию
cp -r .claude/ /tmp/rustafits-claude-backup/

# Также сохрани список файлов для ревью
find .claude/ -type f > /tmp/rustafits-claude-files.txt
cat /tmp/rustafits-claude-files.txt
```

Запиши что именно там лежит — это понадобится на шаге 5.

---

## Шаг 2: Подготовить rustafits repo для submodule использования

В отдельном rustafits репозитории убедись, что:

```bash
cd /path/to/current/rustafits

# 1. Cargo.toml не имеет [workspace] секции
#    (он станет member чужого workspace)
#    Если есть [workspace] — удали её

# 2. Убедись что crate собирается standalone
cargo check
cargo test

# 3. Убери .claude/ из rustafits repo (переедет в athenaeum)
#    Но НЕ делай это до шага 5 — сначала всё скопируй

# 4. Push чистое состояние
git add -A && git commit -m "prepare for submodule usage"
git push origin main
```

---

## Шаг 3: Добавить rustafits как submodule в Athenaeum

```bash
cd /path/to/athenaeum

# Добавить submodule
git submodule add git@github.com:eg013ra1n/rustafits.git rustafits

# Проверить
cat .gitmodules
# Должно быть:
# [submodule "rustafits"]
#     path = rustafits
#     url = git@github.com:eg013ra1n/rustafits.git

# Убедись что код на месте
ls rustafits/src/
```

---

## Шаг 4: Настроить Cargo workspace

### 4a. Создать корневой workspace Cargo.toml

Если в корне athenaeum уже есть Cargo.toml (от Tauri), его нужно заменить на workspace root. Tauri проект переезжает в `src-tauri/` (обычно он уже там).

```toml
# athenaeum/Cargo.toml — WORKSPACE ROOT
[workspace]
resolver = "2"
members = [
    "src-tauri",
    "rustafits",
    # Добавятся позже:
    # "crates/athenaeum-worker",
    # "crates/athenaeum-proto",
]

# Общие зависимости — версии определяются один раз
[workspace.dependencies]
anyhow = "1"
rayon = "1.10"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
nalgebra = "0.33"
thiserror = "2"
log = "0.4"
```

### 4b. Обновить src-tauri/Cargo.toml

```toml
# athenaeum/src-tauri/Cargo.toml
[package]
name = "athenaeum-app"
version = "0.1.0"
edition = "2021"

[dependencies]
rustafits = { path = "../rustafits" }
anyhow = { workspace = true }
rayon = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
log = { workspace = true }
# ... остальные Tauri-специфичные зависимости (tauri, tauri-build, etc.)

[features]
integration-tests = []
```

### 4c. Обновить rustafits/Cargo.toml

Добавь workspace dependencies для общих crate. Важно: rustafits должен оставаться публикуемым standalone, поэтому каждая workspace dependency дублируется обычной версией:

```toml
# rustafits/Cargo.toml
[package]
name = "rustafits"
version = "0.2.0"
edition = "2021"
description = "High-performance astronomical image processing library"
license = "MIT OR Apache-2.0"
repository = "https://github.com/eg013ra1n/rustafits"

[dependencies]
anyhow = { workspace = true }
rayon = { workspace = true }
nalgebra = { workspace = true, optional = true }
# ... остальные существующие зависимости

[features]
default = []
astrometry = ["dep:nalgebra"]  # plate solve модуль

[dev-dependencies]
# для тестов
```

**Важный момент:** когда rustafits публикуется на crates.io из standalone repo (не из workspace), `workspace = true` не сработает. Есть два решения:

**Решение A** — `cargo-publish` скрипт, который перед публикацией заменяет `workspace = true` на конкретные версии. Crate `cargo-edit` или `cargo-set-version` может автоматизировать это.

**Решение B** (проще) — в rustafits указывать обычные версии, а в workspace `Cargo.toml` не определять их как workspace dependencies для rustafits:

```toml
# rustafits/Cargo.toml — standalone-compatible
[dependencies]
anyhow = "1"
rayon = "1.10"
nalgebra = { version = "0.33", optional = true }
```

```toml
# athenaeum/Cargo.toml — workspace root
[workspace]
resolver = "2"
members = ["src-tauri", "rustafits"]

[workspace.dependencies]
# только для athenaeum-app и будущих crate
# rustafits использует свои собственные версии
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

**Рекомендация: используй решение B.** Оно проще и не ломает standalone publish. Workspace dependencies — для internal crate (athenaeum-app, worker, proto), а не для публикуемых библиотек.

### 4d. Проверка

```bash
cd athenaeum/

# Workspace собирается
cargo check --workspace

# Тесты проходят
cargo test --workspace

# rustafits собирается и standalone (важно для crates.io)
cd rustafits/
cargo check
cargo test
cd ..
```

---

## Шаг 5: Перенести skills и agents в Athenaeum

```bash
cd athenaeum/

# Создать структуру
mkdir -p .claude/skills

# Скопировать из бэкапа (шаг 1)
cp /tmp/rustafits-claude-backup/skills/* .claude/skills/

# Если AGENTS.md был в rustafits
cp /tmp/rustafits-claude-backup/AGENTS.md .claude/AGENTS.md
```

### 5a. Обновить AGENTS.md

Старый AGENTS.md был для standalone rustafits. Новый покрывает весь workspace. Замени содержимое на версию из requirements doc (секция "Agent Development Strategy"), адаптировав пути:

```markdown
# Agent Rules for Athenaeum / rustafits workspace

## Workspace structure
- `rustafits/` — git submodule, pure library, publishes to crates.io
- `src-tauri/` — Tauri application (coordinator)
- `crates/athenaeum-worker/` — (future) worker binary
- `crates/athenaeum-proto/` — (future) gRPC definitions

## Layer boundary — ENFORCED
... (содержимое из requirements doc)

## Test commands
... (содержимое из requirements doc)

## File location guide
... (содержимое из requirements doc)
```

### 5b. Обновить skill

Skill файл `rustafits-skill.skill` — обнови пути, если они ссылались на `src/`:

```bash
# Проверь что пути в skill корректны
grep -n "src/" .claude/skills/rustafits-skill.skill
# Если ссылки типа "src/analysis/" — замени на "rustafits/src/analysis/"
# Если ссылки типа "crates/athenaeum-app/src/" — добавь Athenaeum-специфичные секции
```

### 5c. Убрать .claude/ из rustafits repo

Теперь skills и agents живут в athenaeum. В rustafits repo они больше не нужны:

```bash
cd rustafits/

# Удали .claude/ из rustafits
rm -rf .claude/

# Добавь в .gitignore чтобы случайно не вернули
echo ".claude/" >> .gitignore

git add -A
git commit -m "move agent config to athenaeum workspace"
git push origin main

# Вернись в athenaeum и обнови submodule pointer
cd ..
git add rustafits
git commit -m "update rustafits: agent config moved to workspace"
```

---

## Шаг 6: Создать директории для тестов и данных

```bash
cd athenaeum/

# Integration тесты
mkdir -p tests/integration

# Тестовые данные
mkdir -p test-data/fits
mkdir -p test-data/reference
mkdir -p test-data/catalogs/test-mini-tycho2

# Будущие crate
mkdir -p crates/athenaeum-worker
mkdir -p crates/athenaeum-proto
```

---

## Шаг 7: Git LFS для тестовых FITS

```bash
cd athenaeum/

# Инициализация LFS
git lfs install

# Трекинг бинарных файлов
git lfs track "test-data/fits/*.fits"
git lfs track "test-data/fits/*.xisf"
git lfs track "test-data/catalogs/**/*.bin"

# Коммит .gitattributes
git add .gitattributes
git commit -m "configure git-lfs for test data"
```

---

## Шаг 8: CI/CD — автосинхронизация rustafits в отдельный GitHub repo

### Проблема

Разработка идёт в monorepo athenaeum. Изменения в `rustafits/` делаются через submodule. Но submodule — это указатель на коммит в **другом** репозитории. Значит, чтобы обновить submodule pointer в athenaeum, нужно сначала push'нуть изменения в rustafits repo.

### Рабочий процесс (ручной, рекомендуемый на старте)

```bash
# 1. Работаешь в workspace
cd athenaeum/rustafits/

# 2. Вносишь изменения в rustafits
git checkout -b feature/astrometry
# ... код ...
cargo test --workspace  # из корня athenaeum!

# 3. Коммит и push В RUSTAFITS REPO
git add -A
git commit -m "feat: add GnomonicProjection"
git push origin feature/astrometry

# 4. Merge в main (через PR на github.com/eg013ra1n/rustafits)
# После merge:
git checkout main
git pull origin main

# 5. Обновить submodule pointer в athenaeum
cd ..  # корень athenaeum
git add rustafits
git commit -m "update rustafits: add GnomonicProjection"
git push origin main
```

### Автоматизация через GitHub Actions

Для более плавного workflow: CI на athenaeum при push детектирует изменения в `rustafits/` и автоматически создаёт PR в rustafits repo.

```yaml
# athenaeum/.github/workflows/workspace-ci.yml
name: Workspace CI

on:
  push:
    branches: [main, develop]
  pull_request:
    branches: [main]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive

      - uses: dtolnay/rust-toolchain@stable

      - name: Check workspace
        run: cargo check --workspace

      - name: Run tests (Level 1 + 2)
        run: cargo test --workspace

      - name: Clippy
        run: cargo clippy --workspace -- -D warnings

  integration-tests:
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
          lfs: true

      - uses: dtolnay/rust-toolchain@stable

      - name: Run integration tests (Level 3)
        run: cargo test --workspace --features=integration-tests
```

```yaml
# rustafits repo: .github/workflows/standalone-ci.yml
# (живёт в github.com/eg013ra1n/rustafits)
name: rustafits CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable

      - name: Check (standalone, no workspace)
        run: cargo check

      - name: Test
        run: cargo test

      - name: Clippy
        run: cargo clippy -- -D warnings

      - name: Doc
        run: cargo doc --no-deps

  # Будущее: publish to crates.io on tag
  # publish:
  #   if: startsWith(github.ref, 'refs/tags/v')
  #   needs: test
  #   runs-on: ubuntu-latest
  #   steps:
  #     - uses: actions/checkout@v4
  #     - uses: dtolnay/rust-toolchain@stable
  #     - run: cargo publish
  #       env:
  #         CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_TOKEN }}
```

### Автоматический sync submodule (опционально, для продвинутого workflow)

Если хочешь, чтобы CI на athenaeum автоматически пушил изменения в rustafits repo, это возможно, но добавляет сложности. Проще и надёжнее — ручной двухшаговый push (push rustafits → update submodule pointer → push athenaeum).

Однако можно автоматизировать проверку, что submodule pointer актуален:

```yaml
# athenaeum/.github/workflows/submodule-check.yml
name: Submodule sync check

on:
  pull_request:
    branches: [main]

jobs:
  check-submodule:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
          fetch-depth: 0

      - name: Verify rustafits submodule points to merged commit
        run: |
          cd rustafits
          SUBMODULE_COMMIT=$(git rev-parse HEAD)
          # Проверяем что этот коммит есть в main ветке rustafits repo
          git fetch origin main
          if ! git merge-base --is-ancestor $SUBMODULE_COMMIT origin/main; then
            echo "ERROR: rustafits submodule points to commit not in rustafits main"
            echo "Push your rustafits changes and merge to main first"
            exit 1
          fi
```

---

## Шаг 9: Создать скелет astrometry модуля

После настройки workspace — первый коммит с пустыми модулями:

```bash
cd athenaeum/rustafits/

mkdir -p src/astrometry

cat > src/astrometry/mod.rs << 'EOF'
//! Astrometry module: plate solving, WCS, coordinate transforms.
//!
//! Pure computational algorithms — no filesystem or network access.

pub mod projection;
pub mod proper_motion;
pub mod pattern;
pub mod ransac;
pub mod transform;
pub mod wcs;

#[cfg(test)]
pub(crate) mod test_utils;
EOF

# Пустые файлы с заглушками
for module in projection proper_motion pattern ransac transform wcs; do
    cat > src/astrometry/${module}.rs << EOF
//! TODO: implement ${module}
EOF
done

cat > src/astrometry/test_utils.rs << 'EOF'
//! Synthetic test data generators for astrometry tests.

/// Generate a synthetic star field with known WCS for testing.
/// Returns (detected_stars, catalog_stars, ground_truth_wcs).
pub fn generate_synthetic_field() {
    todo!("implement synthetic field generator")
}
EOF
```

Добавить `pub mod astrometry;` в `src/lib.rs`:

```bash
# Добавь строку в lib.rs (проверь что не дублируется)
echo 'pub mod astrometry;' >> src/lib.rs
```

Проверь:

```bash
cd ..  # корень athenaeum
cargo check --workspace
```

Коммит в rustafits и обновление submodule:

```bash
cd rustafits/
git add -A
git commit -m "feat: add astrometry module skeleton"
git push origin main

cd ..
git add rustafits .claude/ tests/ test-data/ .gitattributes Cargo.toml
git commit -m "setup workspace: rustafits submodule + agent config + test structure"
git push origin main
```

---

## Итоговая структура

```
athenaeum/
├── .claude/
│   ├── AGENTS.md                     # правила для агентов (workspace-wide)
│   └── skills/
│       └── rustafits-skill.skill     # перенесён из rustafits
├── .github/
│   └── workflows/
│       ├── workspace-ci.yml          # cargo test --workspace
│       └── submodule-check.yml       # verify submodule in sync
├── .gitmodules                       # rustafits submodule
├── .gitattributes                    # LFS tracking
├── Cargo.toml                        # [workspace] root
├── rustafits/                        # git submodule → github.com/eg013ra1n/rustafits
│   ├── .github/
│   │   └── workflows/
│   │       └── standalone-ci.yml     # standalone cargo test + clippy
│   ├── Cargo.toml                    # standalone-publishable
│   ├── src/
│   │   ├── lib.rs
│   │   ├── analysis/                 # existing
│   │   ├── astrometry/               # NEW
│   │   │   ├── mod.rs
│   │   │   ├── projection.rs
│   │   │   ├── proper_motion.rs
│   │   │   ├── pattern.rs
│   │   │   ├── ransac.rs
│   │   │   ├── transform.rs
│   │   │   ├── wcs.rs
│   │   │   └── test_utils.rs
│   │   ├── processing/               # existing
│   │   └── formats/                  # existing
│   └── benches/
├── src-tauri/                        # Tauri app (coordinator)
│   ├── Cargo.toml                    # depends on rustafits = { path = "../rustafits" }
│   └── src/
│       └── services/
│           ├── catalog/              # CatalogEngine (будущее)
│           └── plate_solve.rs        # PlateSolveService (будущее)
├── src/                              # frontend (React/Svelte/etc)
├── crates/
│   ├── athenaeum-worker/             # (будущее) worker binary
│   └── athenaeum-proto/              # (будущее) gRPC proto
├── tests/
│   └── integration/
│       └── plate_solve_real.rs       # Level 3 tests
└── test-data/
    ├── fits/                         # git-lfs
    ├── reference/                    # PixInsight WCS JSONs
    └── catalogs/
        └── test-mini-tycho2/
```

---

## Чеклист

- [ ] Skills и AGENTS.md скопированы из rustafits
- [ ] `git submodule add` выполнен
- [ ] Корневой `Cargo.toml` — workspace
- [ ] `src-tauri/Cargo.toml` зависит от `rustafits = { path = "../rustafits" }`
- [ ] `cargo check --workspace` проходит
- [ ] `cargo test --workspace` проходит
- [ ] `cd rustafits && cargo check` проходит standalone
- [ ] `.claude/` удалён из rustafits repo
- [ ] Git LFS настроен для test-data/
- [ ] GitHub Actions CI для workspace (athenaeum repo)
- [ ] GitHub Actions CI для standalone (rustafits repo)
- [ ] Astrometry module skeleton создан
- [ ] Submodule pointer обновлён в athenaeum

---

## Команда для агента (после выполнения всех шагов)

```
Workspace настроен. Проверь:
1. `cargo check --workspace` 
2. `cargo test --workspace`
3. rustafits/src/astrometry/mod.rs существует и экспортирует модули
4. .claude/AGENTS.md существует и содержит правила
5. .claude/skills/rustafits-skill.skill существует

Если всё ОК — начинай Phase 1a:
Реализуй GnomonicProjection в rustafits/src/astrometry/projection.rs
по спецификации из requirements doc.
```
