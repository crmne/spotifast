{
  "base": "{{ mode }}",
  "colors": {
    "window": "{{ background }}",
    "panel": "{{ mix background foreground 3% }}",
    "surface": "{{ mix background foreground 8% }}",
    "surface_hover": "{{ mix background foreground 12% }}",
    "surface_active": "{{ mix background foreground 18% }}",
    "outline": "{{ mix background foreground 20% }}",
    "text": "{{ foreground }}",
    "secondary": "{{ mix background foreground 70% }}",
    "dim": "{{ mix background foreground 50% }}",
    "accent": "{{ accent }}",
    "accent_hover": "{{ mix accent foreground 15% }}",
    "on_accent": "{{ background }}",
    "danger": "{{ red }}",
    "warning": "{{ yellow }}"
  }
}
