Bittorrent client written from scratch in Rust initially for the purpose of learning Rust.
My definition of "from scratch" here is that I did not use any existing code/libraries for things in the bittorrent spec.  This means for example I have my own bencoding parser/serializer, but I use other libraries where things aren't newly defined in the Bittorrent spec (e.g. using reqwest for HTTP GET calls to the tracker).

The current goal is to use only nonblocking IO using mio.

Current state is that it can download from a swarm, but not upload.

The these are the main resources used.  I did not look at any existing implementations, any resemblance to them is conincidence (you'll have to take my word on that):
https://www.bittorrent.org/beps/bep_0003.html
https://wiki.theory.org/index.php/BitTorrentSpecification
http://bittorrent.org/bittorrentecon.pdf