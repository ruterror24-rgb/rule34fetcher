# Rule34 Fetcher

A small Windows application with a graphical interface for searching by tags and downloading original-quality images and videos from rule34.xxx.

## Preparation

Create an account or log in rule34.xxx.
Generate an API key in your account settings, enter your user ID and the key into the executable file, save the changes, and you’re all set to start using the program.

This is a type of file created when your API key and user ID are saved.
```env
user_id=YOUR_ID
api_key=YOUR_KEY
```

## Launch

In the application window, specify the tags to include, tags to exclude, the maximum number of posts, and the page range (pages are numbered starting from 1; the range is inclusive). As you type two or more characters, the application displays a dropdown menu of site tags, with the number of posts for each tag shown in parentheses; clicking an item in the list adds that tag. Once a tag is added, the program automatically validates the query and sets the end page based on the number of matches (up to 200 pages); this range can be adjusted manually. Click **Check Tags** to view the post count for each individual tag, as well as the total number of results after applying your inclusions and exclusions. The search function becomes available once all specified tags have been verified. During a download, the **Stop Download** button allows you to abort the task and delete any incomplete `.part` files. Excluded tags are added to the search query with a `-` prefix. Up to 100 posts are requested per page. Downloaded files are saved to the `downloads` directory.
