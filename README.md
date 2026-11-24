# Rule34 Fetcher

A small Windows application with a graphical interface for searching by tags and downloading original-quality images and videos from rule34.xxx.

## Preparation

1. Install Rust and Visual Studio Build Tools, including the **Desktop development with C++** component.
2. Copy `r34.env.example` to `r34.env`.
3. In `r34.env`, enter your `user_id` and your actual `api_key` (found in your account settings).

```env
user_id=YOUR_ID
api_key=YOUR_KEY
```

## Launch

Run `cargo run` from the project folder to open the application window. Alternatively, you can build a release version using the command `cargo build --release` and then launch the application by double-clicking `target\release\rule34-fetcher.exe`. The `r34.env` file must remain in the project root.

In the application window, specify the tags to include, tags to exclude, the maximum number of posts, and the page range (pages are numbered starting from 1; the range is inclusive). As you type two or more characters, the application displays a dropdown menu of site tags, with the number of posts for each tag shown in parentheses; clicking an item in the list adds that tag. Once a tag is added, the program automatically validates the query and sets the end page based on the number of matches (up to 200 pages); this range can be adjusted manually. Click **Check Tags** to view the post count for each individual tag, as well as the total number of results after applying your inclusions and exclusions. The search function becomes available once all specified tags have been verified. During a download, the **Stop Download** button allows you to abort the task and delete any incomplete `.part` files. Excluded tags are added to the search query with a `-` prefix. Up to 100 posts are requested per page. Downloaded files are saved to the `downloads` directory.
