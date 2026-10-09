# Lensfun calibration database (vendored)

`db/` contains the [lensfun](https://lensfun.github.io/) calibration
database, version 1 format — per-lens distortion, lateral chromatic
aberration and vignetting measurements contributed by the lensfun
community, plus camera sensor crop-factor data.

Source: lensfun upstream `version_1` snapshot (57 XML files), as shipped
with darktable.

## License

The lensfun database is licensed under **CC-BY-SA 3.0** by its
contributors; the XML headers refer to the GNU LGPL for the lensfun
*library* — the data files themselves carry the Creative Commons
Attribution-ShareAlike licence. Attribution: "Lens distortion database
by the lensfun project (https://lensfun.github.io/)". ShareAlike applies
to the database files, not to application code.

safelight does **not** link or embed the lensfun library — it parses this
data with its own reader (`core/src/lensdb.rs`).
