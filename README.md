Bittorrent client written from scratch in Rust initially for the purpose of learning Rust.
My definition of "from scratch" here is that I did not use any existing code/libraries for things in the bittorrent spec.  This means for example I have my own bencoding parser/serializer, but I use other libraries where things aren't newly defined in the Bittorrent spec (e.g. using reqwest for HTTP GET calls to the tracker).

The current goal is to use only nonblocking network IO using mio.  Why?  Because it's neat and a challenge.  Maybe I'll give up and switch to async at some point.  The general model currently is that the main logic/network IO is all in one thread and we pass off the disk writes to another thread on piece completion.

Currently can download and upload multiple torrents at once, although pieces of that need more testing.  The main pieces of hot logic (downloading blocks) are decently optimized to minimize copying as I spent a while optimizing that with perf.

I've had some super head-scratching bugs that materialized in strange ways, such as accidentially leaving data in the read buffer to be interpreted as other messages which only later on causes things to grind to a halt, as well as some dumb bit/byte math mistakes in the Bitfield message that caused similar issues.

The these are the main resources used.  I did not look at any existing implementations, any resemblance to them is conincidence (you'll have to take my word on that):
https://www.bittorrent.org/beps/bep_0003.html
https://wiki.theory.org/index.php/BitTorrentSpecification
http://bittorrent.org/bittorrentecon.pdf
