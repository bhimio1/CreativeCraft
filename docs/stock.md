# Stock media

The **Stock** page finds free photos, artworks, illustrations, videos, audio and more from open
collections, and saves what you pick into your projects folder, ready for the apps. It's powered by
[StockCraft](https://github.com/bhimio1/stockcraft)'s library (`stockcraft-local`).

Everything listed is free to use for anything, with no account: StockCraft only lists media its
source marks CC0, public domain, dedicated to the public domain by its author, or a US federal
government work. Sources include Openverse (Wikimedia Commons, Flickr, Smithsonian, …), the Art
Institute of Chicago, the Cleveland Museum of Art, the Metropolitan Museum of Art and NASA.

## Using it

- Type in the search box at the top (it searches as you type, or press Enter); the tabs narrow it
  to photos, illustrations, art, videos, audio and so on.
- Click a result for a larger preview, its rights in plain words, a suggested credit line
  (**Copy credit**), a link to the source's page, and every size the source offers with its
  dimensions and file size. Some originals are hundreds of megabytes: pick the size you need.
- **Download** saves into `Stock/` in the projects folder
  (`Stock/media/<source>/<id>/<readable name>`), so it syncs with the rest of your work and shows up
  on the Projects page. **Open** opens it in the app for its kind of file.
- **Downloaded** lists what you have, searchable offline; right-click an item to copy its credit or
  show it in its folder.
- Credits aren't required for CC0 and public domain media, but they're kind, and customary for
  NASA. Some items carry a note (for example, logos and insignia that aren't covered); it's shown
  in red on the item.

## For agents

The hub offers two tools:

- `stock_search { query, kind?, page? }` returns results with source, id, title, rights, credit
  line and the sizes on offer.
- `stock_download { source, id, size? | max_width? }` saves one into `Stock/` and returns its path
  in the projects folder and its credit line. Without a size it picks the largest file no wider
  than 3000 px, so an agent never pulls a 300 MB scan by accident.

So "find a public-domain photo of a lighthouse and put it on my PhotoCraft poster" works end to
end.

From a terminal: `creativecraft-cli stock search <words> [--kind photo]`,
`stock download <source> <id> [size]`, `stock list [words]`.

## Notes

- StockCraft is a pinned git dependency (`Cargo.toml`); move the pin forward deliberately when
  StockCraft changes.
- The library keeps `catalog.json` beside the media. If two computers download stock at the same
  time, sync keeps both versions of the catalog side by side (nothing is lost; the media files
  themselves never conflict).
