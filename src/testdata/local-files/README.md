These FLAC files contain two seconds of generated silence with test tags,
at 44.1 and 48 kHz in stereo and mono. The scanner tests check that every
format supported by the pinned playback decoder is indexed. They contain
no recorded music. Generated with FFmpeg anullsrc, -t 2 -c:a flac.
