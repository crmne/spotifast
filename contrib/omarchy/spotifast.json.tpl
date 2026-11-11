{
  "base": "{{ mode }}",
  "colors": {
    "window": "{{ background }}",
    "panel": "{{ dark_background }}",
    "surface": "{{ mix background foreground 8% }}",
    "surface_hover": "{{ mix background foreground 12% }}",
    "surface_active": "{{ selection }}",
    "outline": "{{ mix background muted 50% }}",
    "text": "{{ foreground }}",
    "secondary": "{{ mix background foreground 70% }}",
    "dim": "{{ mix background foreground 50% }}",
    "accent": "{{ accent }}",
    "accent_hover": "{{ mix accent foreground 15% }}",
    "on_accent": "{{ background }}",
    "danger": "{{ red }}",
    "warning": "{{ yellow }}",
    "overlay": "{{ mix lighter_background foreground 3% }}",
    "shadow": "{{ darker_background }}80",
    "tint": "{{ accent }}12"
  }
}
