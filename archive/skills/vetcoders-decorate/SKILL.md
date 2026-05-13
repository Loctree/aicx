---
name: vetcoders-decorate
version: 1.1.0
description: >
  Visual polish and micro-interaction skill. Detects the user's existing
  design language (colors, fonts, theme) and proposes tasteful enhancements:
  hover effects, scroll behaviors, parallax, animations, responsive
  refinements. Never overrides the user's style -- always works WITHIN it.
  When no existing style is detected, offers to scaffold one.
  Trigger phrases: "decorate", "make it look good", "add polish",
  "smaczki", "micro-interactions", "udekoruj", "dopracuj wizualnie",
  "curb appeal", "interactive demo", "animate", "add hover effects",
  "make it feel nice", "visual polish".
---

# vetcoders-decorate -- Smaczki for Shipped Products

> "Never change the user's colors. Change how their colors FEEL."

The Decorate skill adds visual micro-interactions and polish to
existing projects. It respects the user's design choices and works
within their established palette, fonts, and layout. The skill
PROPOSES enhancements and lets the user pick what to apply.

## Core Rule: Detect, Don't Dictate

Before decorating anything, run style detection:

```
1. SCAN existing CSS variables, theme files, brand colors
2. IDENTIFY the user's palette (primary, secondary, accent, bg, text)
3. IDENTIFY the user's font stack (headings, body)
4. IDENTIFY the user's theme (dark/light/auto)
5. PROPOSE smaczki using THEIR tokens, not ours
6. ASK which enhancements to apply
7. IMPLEMENT only approved changes
```

If no existing style is detected (raw HTML, no CSS vars, default
browser styles), offer to scaffold a minimal design system first --
but present options, don't assume.

## When To Use

- User says "make it look good" or "add some polish"
- After implementation is done and user wants visual refinement
- For showcase pages, demos, landing pages
- When the product works but feels "flat" or "like a prototype"
- In the VetCoders pipeline: after `dou`, before `hydrate`

## Pipeline Position

```
Phase 1 (Build)      Phase 2 (Converge)      Phase 3 (Ship)
init -> workflow ->   marbles loop ->         dou -> [DECORATE] -> hydrate
followup                                            ^^^^^^^^^^^
```

## What Decorate Proposes

The skill scans the project and proposes from this menu of smaczki.
The user picks which ones to apply. Agent can also invent new ones
that fit the project's character.

### Category 1: Hover and Focus Effects

- **3D Card Tilt** -- cards follow cursor with perspective transform
- **Glow on Hover** -- subtle box-shadow using user's accent color
- **Scale + Lift** -- translateY(-2px) + scale(1.01) on interactive elements
- **Border Color Shift** -- border transitions to accent on hover
- **Magnetic Cursor** -- elements slightly attract toward cursor

### Category 2: Scroll Behaviors

- **Scroll Progress Bar** -- thin accent-color line at viewport top
- **Fade-up on Scroll** -- elements reveal with translateY + opacity
- **Parallax Layers** -- background elements shift slower than foreground
- **Sticky Section Headers** -- headers pin during section scroll
- **Counter Animation** -- numbers count up when scrolled into view

### Category 3: Canvas and Animation

- **Solitaire Board** -- hex-grid marble visualization (VetCoders signature)
- **Particle Background** -- subtle floating particles using user's palette
- **Gradient Mesh** -- animated gradient background using user's colors
- **Loading Skeleton** -- content placeholders with shimmer animation
- **Typed Text** -- typewriter effect on hero headlines

### Category 4: Interaction Details

- **Content Selection Control** -- user-select:none on UI, text on code
- **Smooth Scroll** -- scroll-behavior:smooth on html
- **Active States** -- :active transforms for tactile button feedback
- **Focus Rings** -- accessible custom focus styles using accent color
- **Transition Timing** -- consistent easing curves across all animations

### Category 5: Agent's Choice

The agent should ALSO propose something unique and unexpected
that fits the project's specific character. Examples:

- A project about music? Audio-reactive hover effects.
- A dashboard? Subtle data-point highlights on hover.
- A portfolio? Image parallax within cards.
- Documentation? Smooth code block copy animations.

The best smaczek is one the user didn't think to ask for.

## Style Detection Reference

How to detect and extract the user's design language:

```
CSS Variables:     grep for --primary, --accent, --bg, --text, --font
Tailwind Config:   read tailwind.config.js for theme.extend.colors
CSS Files:         extract most-used colors (hex/rgb/hsl)
HTML:              check <link> tags for Google Fonts
Package.json:      check for UI frameworks (shadcn, MUI, Chakra)
Existing Hover:    check what :hover styles already exist
Theme Mode:        check for prefers-color-scheme or .dark class
```

Use the detected values as your working palette. Map them to roles:
- Primary/accent = CTAs, highlights, progress bars
- Secondary = hover states, borders, subtle effects
- Background = canvas backgrounds, overlay colors
- Text = any generated text elements

## VetCoders Reference Palette (fallback only)

When no user style exists and user wants a suggestion:

```
Orange:  #d97757   Blue:   #6a9bcc   Green:  #788c5d
Dark:    #141413   Light:  #faf9f5   Mid:    #b0aea5
Fonts:   Poppins (headings) + Lora (body)
```

This is OUR palette for OUR products. Never apply it to someone
else's project unless they explicitly ask for it.

## Implementation Pattern

```
Step 1: Style Detection
  - Scan CSS vars, config files, stylesheets
  - Extract palette, fonts, theme mode
  - Note existing hover/animation patterns

Step 2: Proposal
  - List 3-5 specific smaczki from the menu above
  - Include 1 agent's choice (something creative)
  - Show what each would look like with THEIR colors
  - Ask user to pick

Step 3: Implementation
  - Apply only selected enhancements
  - Use user's CSS variables/tokens throughout
  - Add new CSS classes (never modify existing ones)
  - Performance: requestAnimationFrame, passive listeners, will-change

Step 4: Review
  - Show before/after
  - Ask if any adjustments needed
  - Verify no user styles were overridden
```

## Decoration Checklist

Before marking decoration complete:

```
[ ] User's existing styles preserved (no overrides)
[ ] Enhancements use user's palette/tokens
[ ] user-select: none on UI chrome, text on copyable content
[ ] Responsive behavior maintained
[ ] Animations use requestAnimationFrame where applicable
[ ] Scroll listeners are passive
[ ] No emojis added (unless user's project uses them)
[ ] Accessibility: focus states, reduced-motion support
[ ] Performance: will-change on animated elements, sprite caching
```

## Philosophy

Decoration is not about YOUR taste. It is about making the user's
taste FEEL more polished.

A good smaczek is invisible until you remove it.
Then the whole page feels flat.

The agent's job: detect what's there, imagine what could be there,
propose it, and implement only what the user wants.

---

*Phase 3 -- Ship (dou -> decorate -> hydrate)*
*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
