// Picture effects and colour. These are taste, not quality: no preset writes them, and changing one keeps the preset.
class ModernMenuEffects extends ModernMenuPage;

function CreateMenuContents()
{
	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	AddLevelSlider(0, "ModernGodRays", 0, 10, 5);
	AddLevelSlider(1, "ModernVignette", 0, 10, 2);
	AddLevelSlider(2, "ModernFilmGrain", 0, 10, 0);
	AddLevelSlider(3, "ModernChromaticAberration", 0, 10, 0);
	AddLevelSlider(4, "ModernSaturation", 0, 10, 5);
	AddLevelSlider(5, "ModernContrast", 0, 10, 5);
	AddLevelSlider(6, "ModernExposure", 0, 10, 5);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Picture Effects"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="Sun rays"
	Labels(1)="Vignette"
	Labels(2)="Film grain"
	Labels(3)="Colour fringe"
	Labels(4)="Colour strength"
	Labels(5)="Contrast"
	Labels(6)="Exposure"

	Helps(0)="Shafts of light around the sun, cut by what stands in front of it. 0 turns them off."
	Helps(1)="Darker corners of the picture. 0 turns it off."
	Helps(2)="A fine moving grain, as on film. 0 turns it off."
	Helps(3)="Colours split a little towards the edges of the picture, as through a lens. 0 turns it off."
	Helps(4)="How strong the colours are. 5 leaves them as they are."
	Helps(5)="The gap between dark and bright. 5 leaves it as it is."
	Helps(6)="How bright the whole picture is. 5 leaves it as it is."
}
