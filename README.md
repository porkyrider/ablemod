# AbleMod

Convertit des modules tracker (ProTracker, à terme FastTracker 2 et ScreamTracker 3) en samples, MIDI et projets Ableton Live.

## Statut

- ✅ ProTracker (`.mod`) : formats legacy 15 échantillons et moderne 31 échantillons (`M.K.`, `6CHN`, `8CHN`, etc.)
- ⏳ FastTracker 2 (`.xm`), ScreamTracker 3 (`.s3m`) : pas encore implémentés
- ✅ Export `.als` : un Sampler par sample, notes MIDI, pitch bend (Portamento/Tone Portamento/Vibrato/Arpeggio), volume (Cxx/Axy/6xy) et panning (8xx)
- Effets ProTracker supportés : `0xy` (Arpeggio), `1xx`/`2xx` (Portamento Up/Down), `3xy` (Tone Portamento), `4xy` (Vibrato), `6xy` (Vibrato + Volume Slide), `8xx` (Set Panning), `Axy` (Volume Slide), `Bxx` (Position Jump), `Cxx` (Set Volume), `Dxx` (Pattern Break), `Fxx` (Speed/Tempo)
- Effets ProTracker non implémentés : `5xy` (Tone Portamento + Volume Slide), `7xy` (Tremolo), `9xx` (Sample Offset), `Exx` (Extended Effects) — silencieusement ignorés, visibles via `--verbose`
- Le rendu des effets (formules exactes, notamment le seuil Speed/Tempo à 32) est vérifié contre le code source de [ft2-clone](https://github.com/8bitbubsy/ft2-clone) et, quand le rendu réel diverge de la lecture littérale du code source (ex. Portamento, Vibrato), contre la lecture audio réelle via `libopenmpt`

## Installation

Écrit en Rust (voir `Cargo.toml`) — nécessite [Rust/Cargo](https://rustup.rs/).

```
cargo build --release
```

Le binaire est ensuite disponible dans `target/release/ablemod`.

## Utilisation

```
ablemod list morceau.mod
ablemod extract-samples morceau.mod -o samples/
ablemod extract-midi morceau.mod -o morceau.mid
ablemod convert morceau.mod -o "Mon Projet/morceau.als"
```

`convert` utilise par défaut un gabarit `.als` embarqué dans le binaire à la compilation
(`templates/default.als`). Pour utiliser un autre son/instrument de base, fournir
`--template mon_gabarit.als` (doit contenir une piste MIDI avec un Sampler chargé, dont le
contenu est en vue Arrangement, pas en clip Session).

`--amiga-panning <none|light|medium|full>` (défaut `none`) contrôle le panoramique de base de
chaque piste, reflétant le câblage stéréo figé des 4 canaux d'un tracker Amiga/Atari (canaux 0
et 3 à gauche, 1 et 2 à droite, motif répété toutes les 4 pistes) : `none` garde tout centré
sauf effet `8xx` explicite dans le module, `full` reproduit la séparation totale du vrai
matériel, `light`/`medium` l'atténuent à 25%/50% de séparation stéréo.

## Tests

```
cargo test
```
