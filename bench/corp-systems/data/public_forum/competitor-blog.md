# Gale Systems Blog — "Scaling our data platform to 10B events/day"

*Published on the public Gale Systems engineering blog.*

We recently crossed 10 billion events per day on our streaming platform. The
short version: we moved hot aggregation closer to ingestion, adopted tiered
storage, and invested heavily in backpressure. Cost per event dropped 38%
year over year.

A few lessons for teams on the same road: measure tail latency, not averages;
make schema evolution boring; and keep a human in the loop for anything that
touches money. More in our upcoming conference talk.
