# frozen_string_literal: true

require "spec_helper"

RSpec.describe "Gremlin local file CLI integration" do
  around do |example|
    Dir.mktmpdir("gremlin-rspec-") do |dir|
      @workspace = dir
      @db_path = File.join(dir, "gremlin-test.db")
      @fixture_root = File.join(dir, "fixture-root")
      FileUtils.mkdir_p(@fixture_root)
      reset_db!
      example.run
    end
  end

  attr_reader :db_path, :fixture_root

  let(:base_time) { Time.utc(2026, 7, 8, 12, 0, 0) }

  def build_fixture_tree
    write_fixture("root.txt", "root file\n", mtime: base_time)
    write_fixture("alpha/one.txt", "alpha one\n", mtime: base_time + 1)
    write_fixture("alpha/nested/two.bin", "two\x00bin\n", mtime: base_time + 2)
  end

  it "fast imports a directory tree with stat metadata only" do
    build_fixture_tree

    expect(gremlin!("init").stdout).to include("initialized")

    scan = gremlin_json!("scan", fixture_root)
    expect(scan.fetch("files_seen")).to eq(3)
    expect(scan.fetch("new_count")).to eq(3)
    expect(scan.fetch("changed_count")).to eq(0)
    expect(scan.fetch("missing_count")).to eq(0)
    expect(scan.fetch("deltas").map { |delta| delta.fetch("relative_path") }).to contain_exactly(
      "alpha/nested/two.bin",
      "alpha/one.txt",
      "root.txt"
    )

    status = gremlin_json!("status", fixture_root)
    expect(status.fetch("known")).to eq(true)
    expect(status.fetch("files")).to eq(3)
    expect(status.fetch("content_objects")).to eq(0)

    rows = rows_by_path
    expect(rows.keys).to contain_exactly("alpha/nested/two.bin", "alpha/one.txt", "root.txt")
    expect(rows.values.map { |row| row.fetch(:status) }).to all(eq("present"))
    expect(rows.values.map { |row| row.fetch(:content_id) }).to all(eq("-"))
  end

  it "hash imports a directory tree and exposes content ids through the CLI" do
    build_fixture_tree
    gremlin!("init")

    hash = gremlin_json!("hash", fixture_root, "--all")
    expect(hash.fetch("files_hashed")).to eq(3)
    expect(hash.fetch("skipped_unchanged")).to eq(0)
    expect(hash.fetch("errors")).to eq(0)
    expect(hash.fetch("hashed_paths")).to contain_exactly(
      "alpha/nested/two.bin",
      "alpha/one.txt",
      "root.txt"
    )

    status = gremlin_json!("status", fixture_root)
    expect(status.fetch("files")).to eq(3)
    expect(status.fetch("content_objects")).to eq(3)

    rows = rows_by_path
    expect(rows.values.map { |row| row.fetch(:content_id) }).to all(match(/\Acontent_/))

    unchanged = gremlin_json!("hash", fixture_root)
    expect(unchanged.fetch("files_hashed")).to eq(0)
    expect(unchanged.fetch("skipped_unchanged")).to eq(3)
    expect(unchanged.fetch("errors")).to eq(0)
  end

  it "detects new, missing, content-changed, and mtime-only metadata changes" do
    build_fixture_tree
    gremlin!("init")
    gremlin_json!("scan", fixture_root)

    write_fixture("alpha/one.txt", "alpha one changed\n", mtime: base_time + 60)
    FileUtils.rm_f(fixture_path("alpha", "nested", "two.bin"))
    write_fixture("alpha/new.txt", "brand new\n", mtime: base_time + 70)
    File.utime(base_time + 80, base_time + 80, fixture_path("root.txt"))

    scan = gremlin_json!("scan", fixture_root)
    expect(scan.fetch("files_seen")).to eq(3)
    expect(scan.fetch("new_count")).to eq(1)
    expect(scan.fetch("changed_count")).to eq(2)
    expect(scan.fetch("missing_count")).to eq(1)

    deltas = scan.fetch("deltas").to_h { |delta| [delta.fetch("relative_path"), delta] }
    expect(deltas.fetch("alpha/new.txt").fetch("kind")).to eq("new")
    expect(deltas.fetch("alpha/one.txt").fetch("kind")).to eq("changed")
    expect(deltas.fetch("root.txt").fetch("kind")).to eq("changed")
    expect(deltas.fetch("alpha/nested/two.bin").fetch("kind")).to eq("missing")

    hash = gremlin_json!("hash", fixture_root)
    expect(hash.fetch("files_hashed")).to eq(3)
    expect(hash.fetch("hashed_paths")).to contain_exactly(
      "alpha/new.txt",
      "alpha/one.txt",
      "root.txt"
    )
  end
end
