# FerriumWM (FeWM)

Тайлинговый Wayland-композитор на Rust + [Smithay]. 

## Что умеет

- **Раскладки**, переключаются на лету и задаются по рабочим столам:
  - `dwindle` — бинарное дерево, как в Hyprland;
  - `master` — master/stack, мастер слева/справа/сверху/снизу/по центру;
  - `scroller` — бесконечная лента колонок как в niri, горизонтальная **или вертикальная**,
    с настраиваемой скоростью прокрутки колесом и жестами, центрированием и пресетами ширины;
  - `floating`.
- **Конфиги на трёх языках**: `~/.config/FerriumWM/configs/config.py`, `config.lua`, `config.sh`.
  Правишь любой — при сохранении он выполняется своим интерпретатором, проверяется,
  остальные два переписываются, а FeWM применяет изменения без перезапуска.
- **CSS для внешнего вида**: `~/.config/FerriumWM/style.css` — рамки (в т.ч. градиентные),
  отступы, тени, затемнение, прозрачность, размытие фона, по умолчанию без скруглений
  (`--radius: 0px`). Селекторы по состоянию (`:focus`, `:floating`, `:fullscreen`, `:urgent`)
  и по окну (`window[app_id~="kitty"]`, `[title*="vim"]`), переменные палитры `var(--primary)`.
- **Анимации**: открытие окон (popin/slide/fade), перемещение, смена столов
  (slide/slide-vertical/fade), прокрутка ленты, смена цвета рамки, смена обоев.
  Кривые: пресеты, `bezier(...)`, `spring(затухание, жёсткость)`.
- **Обои и цвета**: обои рисует сам композитор; из них генерируется палитра Material You
  (схемы content, tonal-spot, vibrant, ...), которая разливается по приложениям через шаблоны
  в синтаксисе matugen/noctalia (`{{colors.primary.default.hex | darken 0.1}}`).
  При первом запуске импортируются шаблоны из noctalia (gtk, qt, kitty, btop, kde, pywalfox,
  telegram, vesktop, ...).
- **Хоткеи**: при первом запуске импортируются из `~/.config/niri/config.kdl`; названия действий
  niri (`focus-column-left`, `consume-or-expel-window-left`, `move-column-to-workspace 3`, ...)
  понимаются напрямую, плюс свои (`focus left`, `layout scroller`, `wallpaper-random`, ...).
  Хоткеи не зависят от раскладки клавиатуры.
- Протоколы: xdg-shell, layer-shell (бары, лаунчеры, уведомления), xdg-decoration,
  linux-dmabuf, fractional-scale, viewporter, буфер обмена (+primary, data-control),
  pointer-constraints/relative-pointer/gestures, cursor-shape, xdg-activation,
  ext-session-lock (swaylock), X11-приложения через xwayland-satellite.
- **Скриншоты**: `ext-image-copy-capture-v1` (grim ≥ 1.5, flameshot с `useGrimAdapter=true`),
  плюс действия `screenshot`, `screenshot-screen`, `screenshot-window`. Копирование
  фреймбуфера распараллелено по ядрам.
- **Многоядерность**: тяжёлые CPU-задачи (копирование фреймбуфера при скриншоте,
  построение dark/light палитры, чтение/рендер/запись шаблонов) выполняются
  параллельно по всем ядрам; кадр при этом рисуется на GPU в главном потоке.
- IPC: `fewm msg <действие>`, `fewm msg windows|workspaces|outputs|focused|colors|events`.

## Запуск

```sh
cargo build --release
install -Dm755 target/release/fewm ~/.local/bin/fewm
install -Dm755 resources/fewm-session ~/.local/bin/fewm-session

fewm --nested            # окном внутри текущей сессии (для проб)
fewm-session             # с TTY
sudo install -Dm644 resources/ferriumwm.desktop /usr/share/wayland-sessions/ferriumwm.desktop  # для sddm
```

Лог сессии: `~/.cache/FerriumWM/session.log`. Подробные логи: `FEWM_LOG=debug`.

## Пока не сделано

- Обзор рабочих столов (`toggle-overview`), анимация закрытия окон.
- Рабочие столы в баре noctalia (нужна поддержка FeWM-IPC на стороне бара).
- Несколько GPU (рендер на одном основном).

[Smithay]: https://github.com/Smithay/smithay
